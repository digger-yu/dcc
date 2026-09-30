# P2-1 SSA Pass 基础设施完整升级方案

> **本方案的完成度表已按实测修正。**下表是写方案时的声称值与实测值的差异；
> 执行结果见末尾「执行记录」一节。

| P# | Pass | 方案原表 | 实测（修前） | 当前状态 |
|----|------|--------|------------|---------|
| P1 | cross_function_dce | ✅ 完整 | 真 | ✅ |
| P2 | strength_reduction | ✅ 完整 | 真 | ✅ |
| P3 | ivop | ✅ 完整 | 真 | ✅ |
| P4 | alias_analysis | ✅ 完整 | **返回值被丢弃，对产物零影响** | ⚠ 仍为分析结果未接入 |
| P5 | instruction_scheduling | ✅ 完整 | 真（依赖图驱动） | ✅ |
| P6 | pgo | ✅ 完整 | **hint 在重建时被丢弃 → 空转** | ⚠ 待接真消费者 |
| P7 | auto_parallelization | ✅ stub+hint | 空转，且因循环闸门**根本跑不到** | ⚠ |
| P8 | loop_tiling | ✅ 完整 | 同上（空转） | ⚠ |
| P9 | loop_interchange | ✅ 完整 | 同上（空转） | ⚠ |
| P10 | tail_call_optimization | ✅ 完整 | 真 | ✅ |
| P11 | function_attribute_propagation | ✅ 完整 | **假 hint 导致 -O2 SEGV** | ✅ 已删除（段归属回到 AST 真源） |
| P12 | register_allocation | ⚠ stub | 只统计、后端不读、还误报 `changed=true` | ❌ **未做**（需后端级重写，见执行记录） |
| P12b | ubsan_ir_instrument | ✅ 完整 | guard 已插；文档声称的符号转换代码不存在 | ⚠ 注释已修正 |
| P13 | alias_set | ✅ 完整 | 返回值被丢弃 → 空转 | ⚠ |
| P14 | speculative_execution | ✅ 完整 | **按 dest 字典序重排 → -O3 静默误编译** | ✅ 已重写为依赖闭包上提 |
| P15 | SCCP | ✅ 完整 | 真 | ✅ |
| P16 | software_pipelining | ⚠ stub | 空函数体却 `modified=true`；被循环闸门结构性锁死 | ❌ **未做**（已删 stub，前置链已记录） |
| P17 | GVN | 缺失（仅 AST 层同名物） | — | ✅ **本轮新增（SSA 层）** |
| P18 | DSE | 缺失（`prune_dead_stores` 不是 DSE） | — | ✅ **本轮新增（SSA 层）** |
| P19 | jump threading | 缺失 | — | ✅ **本轮新增（SSA 层）** |

**为何必须先把虚高的完成度改下表**：一个不影响产物的 pass 不叫「stub 待完善」，叫死代码；
把它记作 ✅ 会直接误导后续所有判断（本轮就是先改了 6 个静默误编译才能安全动优化器）。

---

## 当前状态分析

### 已完成
1. 基础设施：`struct Function/BasicBlock/Inst/InstKind/Terminator` 已经定义完整。
2. 基础算法：`compute_dominators`，`insert_loop_phis`，AST→SSA 构建，SSA→AST 重建。
3. 守门人 `is_structured`：识别纯结构化控制流函数，跳过 goto/label/switch/asm goto。
4. 已实现 16 个 pass，大部分已经可用：
   | P# | Pass | 完成度 | 说明 |
   |----|------|--------|------|
   | P1 | cross_function_dce | ✅ 完整 | 跨函数不可达删除 + 函数内死指令 |
   | P2 | strength_reduction | ✅ 完整 | 乘/除幂次 2 → 移位 |
   | P3 | ivop | ✅ 完整 | 归纳变量重写，给强度削减用 |
   | P4 | alias_analysis | ✅ 完整 | Steensgaard union-find |
   | P5 | instruction_scheduling | ✅ 完整 | BB 内 list scheduling |
   | P6 | pgo | ✅ 完整 | 读取 PGO 数据，传递 hot/cold hint |
   | P7 | auto_parallelization | ✅ stub+hint | 识别可并行循环，插入 `#pragma omp simd` |
   | P8 | loop_tiling | ✅ 完整 | 简单 for 循环分块 |
   | P9 | loop_interchange | ✅ 完整 | 嵌套循环交换，插入 hint |
   | P10 | tail_call_optimization | ✅ 完整 | 自递归尾调用识别，后端提示 |
   | P11 | function_attribute_propagation | ✅ 完整 | noreturn/cold/hot 属性传播 |
   | P12 | register_allocation | ⚠️ stub/hint | 线性扫描框架，只是 hint 给后端 |
   | P12b | ubsan_ir_instrument | ✅ 完整 | UBSAN 在 IR 层插桩 |
   | P13 | alias_set | ✅ 完整 | 基于别名分析的三档区分 |
   | P14 | speculative_execution | ✅ 完整 | 无副作用分支外提 |
   | P15 | sparse_conditional_constant_propagation | ✅ 完整 | SCCP 常量传播 + 死 BB 删除 |
   | P16 | software_pipelining | ⚠️ stub | 占位，未实现真正流水调度 |

### 缺失/不完整
1. **P0-4 遗留：非结构化 goto/label/switch 支持**：当前 `is_structured` 守门人直接跳过，需要更深层 CFG 重构才能支持。
2. **P12 寄存器分配**：当前是 stub/hint，物理寄存器分配仍由 codegen 的硬编码 GP/XMM 寄存器池处理，没有真正 SSA 层面分配。
3. **P16 软件流水**：占位 stub，未实现。
4. **缺失 GVN**：全局值编号，用于消除冗余计算和 CSE。
5. **缺失 dead store elimination**：删除对变量无用的写入，减少内存访问。
6. **缺失 jump threading**：通过常量传播优化跳转链，消除冗余分支。

---

## 目标范围

### P2-1 本次升级范围
1. 不碰 P0-4 遗留问题：goto/label/switch 的 CFG 重构仍留作未来工作，保持现状（非结构化函数跳过 SSA）。
2. 完成现有 stub 补全：
   - P12：完整线性扫描寄存器分配（替代后端硬编码池）。
   - P16：完整软件流水调度（O3 级）。
3. 新增两个基础优化 pass：
   - GVN：全局值编号 + 冗余计算消除。
   - DSE：死存储消除。
4. 可选增量：jump threading（如果时间允许）。

### 不做范围
- goto/label/switch 的非结构化 CFG 支持 → P0-4 独立大型项目，仍留作未来工作。
- 循环克隆/不变式外提等循环优化 → 已经在 loop_opt.rs，不在 SSA pass。

---

## 设计原则

保持 suckless 原则：
- 零外部依赖，单一文件实现。
- 渐进式改进，不破坏现有结构和接口。
- stub → 完整实现 增量式替换，保持可测试性。
- 保持现有 `is_structured` 守门人，非结构化函数仍跳过。

---

## 详细实施方案

### 阶段 1：补全 P12 寄存器分配（预计 3-5 天）

#### 当前问题
- 当前 `register_allocation` 只是一个框架占位，生成 hint 给 codegen，但实际分配仍在 codegen 硬编码。
- 硬编码分配不利用 SSA 生命周期信息，导致寄存器浪费。
- 需要真正线性扫描分配，和现有 infrastructure 集成。

#### 实施要点

1. **生命周期计算**
   - 对每个 SSA 变量计算 `start_bb` 和 `end_bb`，得到使用区间 `[start, end]`。
   - 在 `insert_loop_phis` 后计算，复用 dominator 信息。

2. **线性扫描核心算法**
   - 按 BB 顺序处理所有区间，维护活跃区间列表。
   - 当新区间无法找到空闲寄存器，启发式溢出最长区间到栈。
   - 复用现有的 `PhysReg` 枚举（GP + XMM），提供给 codegen 使用。

3. **SPILL/RELOAD 插入**
   - 溢出后在 `use` 点插入 reload，`def` 点插入 spill。
   - 栈帧已经对齐，只需要在 frame 分配溢出槽，记录偏移。

4. **与 codegen 集成**
   - SSA IR 层面分配完成，重建 AST 时每个 `Ident` 携带分配的物理寄存器编号。
   - codegen 不再分配，直接使用编号。
   - 保持向后兼容：如果不跑 SSA，仍用原来硬编码分配。

5. **测试**
   - 现有测试全量回归，检查寄存器分配正确性。
   - 对比 -O2 和 -O0 运行结果，验证输出一致。

---

### 阶段 2：补全 P16 软件流水（预计 3-5 天）

#### 当前问题
- 当前是占位 stub，在 `-O3` 调用但不做任何事情。
- 目标：对短迭代循环做软件流水，重叠不同迭代的执行，提高指令级并行。

#### 实施要点

1. **前置条件检查**
   - 只处理已知 trip count，归纳变量线性增长的 innermost 循环。
   - 循环体大小适中（< 64 条指令），指令级并行空间足够。

2. **经典迭代间隔调度算法**
   - 计算依赖图，找到关键路径长度得到最小迭代间隔 (`MII`)。
   - 尝试从 `MII` 开始找可行调度，逐步增加间隔直到找到解。
   - 最终产生 prologue + kernel + epilogue。

3. **现有 IR 改造**
   - 不需要改 SSA IR 结构，只需要重新排列 BB 和指令，插入 phi。
   - 复用现有的循环信息从 ivop 得到。

4. **测试**
   - 对不同迭代间隔测试调度正确性。
   - 运行现有循环基准，验证结果正确。

---

### 阶段 3：新增 P17 GVN 全局值编号（预计 4-7 天）

#### 目标
- 对相同值的计算只执行一次，消除冗余计算。
- 支持简单常量折叠和谓词引导常量替换。

#### 实施要点

1. **RPO 遍历每个 BB**
   - 对每个指令计算其 value number：
     - 如果操作数都是常量，常量折叠。
     - 如果操作数 value number 和已有指令相同，复用已有结果，删除当前指令。
   - 使用哈希表 `(opcode, value numbers of operands) -> value number`。

2. **处理 phi 节点**
   - 两个 phi 所有输入 value number 相同，则合并为同一个 value number。
   - 需要迭代直到不动点。

3. **谓词信息处理 (GVN-PRE)**
   - 可选：基于支配关系的部分冗余消除 (PRE)。
   - 如果时间不够，先做基本 GVN，PRE 留作后续优化。

4. **集成到现有 pipeline**
   - 放在 SCCP 之后，调度之前。
   - 因为 SCCP 已经做了常量传播，GVN 可以基于常量做更多合并。

5. **测试**
   - 添加测试用例 `tests/ssa/gvn.c`，验证冗余计算被消除。
   - 全量回归测试确保不破坏已有优化。

---

### 阶段 4：新增 P18 DSE 死存储消除（预计 2-4 天）

#### 目标
- 删除对局部变量没有后续读取的写入，减少内存访问和指令数。

#### 实施要点

1. **每个 BB 反向扫描**
   - 从 BB 底部向上扫描，跟踪活跃的写入。
   - 如果一个存储对应当前没有活跃读取，删除它。
   - 遇到读取就标记地址为活跃，遇到存储就删除未读取的旧存储。

2. **volatile 感知**
   - volatile 存储不删除，无论是否读取。
   - 基于指令 `is_volatile` 标记判断，依赖之前的 volatile 修复。

3. **跨 BB DSE**
   - 先用局部 DSE，再基于 dominator 做全局 DSE。
   - 如果存储在所有后继路径都被覆盖，且无读取，则删除。

4. **集成到 pipeline**
   - 放在 DCE 之后，别名分析之后，所以别名信息可用。
   - 利用别名集合判断是否可能被读取。

5. **测试**
   - 添加测试用例验证死存储被删除，volatile 保留。
   - 全量回归。

---

### 阶段 5：（可选）新增 P19 Jump Threading（预计 3-5 天）

#### 目标
- 通过常量传播消除冗余分支和跳转，合并连续跳转链。

#### 实施要点

1. **识别可优化跳转**
   - 当分支条件是常量，直接跳转到对应目标 BB。
   - 当跳转目标的后继还是跳转，合并直接跳到最终目标。

2. **更新 CFG 和 dominator**
   - 移除不可达 BB，更新边和前驱后继。
   - 重新计算 dominator。

3. **迭代处理**
   - 一轮处理可能产生新的可优化跳转，迭代直到不动点。

4. **测试**
   - 测试常量分支消除。
   - 测试跳转链合并。

---

## 调度和提交策略

### 分阶段独立提交，每个阶段可单独验证：

| 阶段 | 任务 | 预估时间 | 可独立测试提交 |
|------|------|----------|----------------|
| 1 | P12 补全寄存器分配 | 3-5 天 | ✅ 是 |
| 2 | P16 补全软件流水 | 3-5 天 | ✅ 是 |
| 3 | 新增 P17 GVN | 4-7 天 | ✅ 是 |
| 4 | 新增 P18 DSE | 2-4 天 | ✅ 是 |
| 5 | (可选) 新增 P19 Jump Threading | 3-5 天 | ✅ 是 |

### 每个阶段验收标准
- 该阶段功能通过新增单元测试。
- 现有全部测试通过，不引入回归。
- objdump 验证生成机器码优化正确。

---

## 风险和依赖

### 依赖
- DSE 依赖 volatile 修复（之前方案已经增加 `is_volatile` 标记）。
- GVN 依赖 SCCP 已经提供常量信息。
- 寄存器分配依赖 loop 信息已经在 SSA 构建正确。

### 风险缓解
- 每个 pass 默认在 `-O2+` 开启，但可以通过命令行关闭回退到 stub。
- 如果某个 pass 引入回归，可单独 revert 不影响其他 pass。
- 保持原接口不变，增量修改，不破坏现有结构。

---

## 最终产物

- `src/ssa_passes.rs` 完整 20 个 pass，所有 stub 补全。
- 新增两个 pass 文件（或在现有文件增加），保持单一文件原则。
- 新增测试用例覆盖每个新功能。
- 更新 `docs/architecture.md` 描述新 pass 列表和调度。

---

## 执行记录（本轮）

### 1. 前置：先修完阻塞验收的 6 个静默误编译

方案阶段 1 的验收标准是「对比 -O2 和 -O0 运行结果一致」，而该前提**当时就不成立**：
`int f(int n){int s=0;while(n>0){s+=n;n--;}return s;}` 在 -O2 就是死循环。
以下每一条都有可复现夹具与单元/集成测试，均与 gcc 交叉验证：

| # | 缺陷 | 位置 |
|---|---|---|
| A | LICM：`Stmt::While` 分支传 `loop_var = None`，`expr_mentions(_, None)` 恒 false ⇒ 整个循环体被判不变式外提 ⇒ 空转死循环；外提语句又被 prepend 到块首破坏「顺序单一真源」 | `src/loop_opt.rs` |
| B | 并行 codegen 每个 chunk 各自 `next_label: 0`，合并时 `labels.insert` **同名静默覆盖** ⇒ 跳进隔壁函数；并行产物≠串行产物 | `codegen/x86_64/parallel/orchestration.rs` |
| C | AST 层 GVN 无 kill 机制：常量被当「可用表达式」替成同值的可变变量（`int s = 0;` → `int s = acc;`），且 avail 表只增不减 | `src/ssa_gvn.rs` |
| D | `coalesce_block` 的别名重写不递归 `Expr::Call` 被调者 ⇒ `h(3,4)` 里的 `h` 未改名而声明已删，后端当外部符号（`undefined reference to 'h'`） | `src/ssa_gvn.rs` |
| E | SSA 构建器给**文件作用域对象**开 SSA 版本，版本名在重建时被合成本地声明而遮蔽全局 ⇒ `bump()` 写的是本帧栈槽，全局永不更新 | `src/ssa.rs` |
| F | `lower_expr_into` 违约：`Ident`/`String`/`Char` 分支只返回裸操作数不物化 ⇒ 函数指针局部变量的**初始化彻底消失**（读未初始化栈槽→SEGV）；附带修了 `Type::Func` 不衰减导致的 64 位地址截成 32 位 | `src/ssa.rs` |
| G | P14 `speculative_execution` 按 dest 名字**字典序**重排块内指令（注释称「无依赖冲突」）⇒ 使用跑在赋值之前，-O3 错值 | `src/ssa_passes.rs` |
| H | P11 假 hint （`__dcc_section_hot/cold`）不在重建丢弃名单里 ⇒ 对不存在符号的内存写被后端回退成栈槽 → **-O2 SEGV** | 已删 pass |
| I | `func_has_loop` 闸门无 `_ => false` 之外的事件：漏 `Stmt::Switch`/`Label`/语句表达式/嵌套函数 ⇒ switch 里的循环漏进 SSA（HANG/SEGV/错值） | `src/opt.rs` |
| J | fail-fast 缺失：`sym_index.get(..).unwrap_or(0)` 把「无此符号」静默降级成 `STN_UNDEF`（重定位落到镜像起点）；已统一为单一解析路径 + E1103，并补齐 UND 登记（asm 外部符号）与数字字面量不再被误当符号 | `codegen/x86_64/elf/{resolve,rel_symbols}.rs` |

### 2. 方案阶段 3/4/5：已完成（SSA 层新 pass）

- **P17 `gvn`**：整型同形计算按 `in[BB] = ⋂ out[pred]` 跳块归一（SSA 名不可变，无需 kill）；
  内存读与浮点运算只在本块内复用且遇任何写/调用即作废；`fenv_access` 为真时浮点完全不参与；
  phi 退化与同块合并；迭代到不动点（上限 16）。
- **P18 `dead_store_elim`**：位置模型 `Named/Via/Opaque`；局部覆盖 + 沿「后继唯一前驱」链的全局判定；
  volatile 写、文件作用域对象写、地址逃逸的对象写一律保留（宁少删不错删）。
- **P19 `jump_threading`**：常量分支折叠、空跳板链合并、不可达块剔除，并修补 CFG 缓存与
  剩悬空前驱行的 phi（漏修则会读一个永不赋值的局部）。
- 已接入 `run_with_opts`（按方案要求的顺序：DSE 在别名分析后、GVN 在 SCCP 后、穿线再随后并清死代码），
  均带单元测试（`cargo test --lib ssa_passes` 32 项）。
- 三个新 pass 接入后的全量扫结果：异常夹具条目 **326 → 197**（净修复 131 项），
  零新增回归（仅剩下表那条存量项）。

### 3. 方案阶段 1/2（P12 / P16）：**未做**，原因不是时间不够而是前提不成立

- **P12**：方案原文要求「重建 AST 时每个 `Ident` 携带物理寄存器编号、codegen 不再分配」。
  实测：`ast::Ident` 无寄存器字段；三个后端的变量模型全是「栈槽 + 固定累加器」
  （x86_64 `var_map` + `%r13+disp` + `%rax`；aarch64 `x0/w0`；riscv64 `a0/a1`），
  `ArchRegPool` 只服务 inline asm 绑定、连 x86_64 pool 都没有。
  ⇒ 这不是 pass 级补全而是**后端级重写**（需单独立项，含与 golden baseline / 可复现门禁逐字节对撞）。
  原 stub （只发 `__dcc_regalloc_done` 统计、后端从不读、还误报 `changed=true`）已删除，
  理由写在下文 P12 小节。
- **P16**：当时结构性不可行 —— `run_ssa_passes` 对所有含循环函数 `continue`，P16（以及 P7/P8/P9）
  对任何真实代码都是死代码。原空函数体 stub（无动作却 `modified=true`）已删除。
  **⇒ 该前提已在 §6 随闸门删除而消失，P16 现在是必须真做出来的项（待办）；
  但已知的一个 4 层嵌套形态 -O2 误编译（见 §6 末表）会干扰它的验收，需先修那一项。**

#### 解锁循环进 SSA 管线的当前状态（本轮实测推进）

本轮加了诊断开关 `DCC_SSA_LOOPS=1`（强行让含循环的函数也进 SSA）来量化真实差距，
并在此过程中发现**循环根本没在真正的 SSA 形式上**：

| 发现 | 证据 | 处置 |
|---|---|---|
| `insert_loop_phis` 只从 **header 块**收集使用，而 `while` 的 header 常常只有 cond；且插了 phi 也**不改写体内引用** | `phi i_3 <- [(i_3, …)]` 自引用 / 循环累加读的是进循环前的版本 | 已重写：循环体全域定义/使用集合 + 体内引用重命名到 phi dest + 记真实 preheader（取消魔法串 `"__entry"`） |
| SCCP 把循环携带变量的初值当全函数常量传进体里 | `int s=0;while(n>0){s+=n;…}` 在解锁后返回垃圾值 | 已修：参与 phi 的名字与**多版本基名**一律不入常量表 |
| 块内重排可以「同基名不同版本」为由颠倒同一变量的两次赋值 | 本轮自己引入的回归：`y=5;y=20;y+=3` 变 5 | 已修：上提不得跨过同基名写入/读对方基名；常驻夹具 `ssa_order_multi_assign.c` |

量化结果（同一套 `-O0 ≡ -O2 ≡ -O3` 夹具扫描）：

| 配置 | OPT-DIFF 项 |
|---|---|
| 本轮修正前（默认，闸门关） | 135 |
| 本轮修正后（默认，闸门关） | **107** |
| 本轮修正后（**解锁循环**） | **107**（与不解锁持平） |

⇒ 解锁在 326 个夹具语料上已**零额外失效**，但仍未取消闸门：本地最小夹具
`tests/fixtures/opt/ssa_loop_shapes.c` 暴露出一个残留缺陷 —— `for` 的**入口守卫**在
「循环头自身不是条件块」的形态下会丢失，`nest(0)`（零次迭代）多跑一圈返回 1。
因此闸门保留为默认开启、`DCC_SSA_LOOPS=1` 保留为诊断/度量开关。

解锁余项已定位的具体机制（按修完后的优先序）：

1. **入口守卫**（当前唯一阻断解锁的项）：构建器对 `for` 同时在 `for.cond`（顶部）与
   `for.step`（底部）各放一次条件判断，而 `for.cond` 的 terminator 最终是 `Jump`；
   重建时 `rebuild_cycle` 以 `for.body` 为循环头并得出 `cond_expr = None`，于是
   `for.cond` 里那个**只在入口处生效**的测试被当成孤儿比较丢弃 ⇒ 零次迭代跑一次。
   修向：`rebuild_cycle` 在 `cond_expr.is_none()` 时必须检查 preheader（或循环体外任何
   支配循环头的前驱）是否带一个「一进体、一出循环」的 Branch，并把它发成循环外的
   入口 `if`（或在构建器里不再重复放置底部测试）。
2. 入口守卫修完后：把 `run_ssa_passes` 里的 `DCC_SSA_LOOPS` 判定删掉，并删掉
   `func_has_loop` 专用判据（`func_has_loop`/`block_has_loop`/`stmt_has_loop`/`expr_has_loop`）。
3. 然后才能真做 P16（软件流水）。

已经完成的解锁前置（均有测试或语料证据）：

- `insert_loop_phis`：从**整个循环体**收集定义与使用、把体内引用重命名到 phi dest、
  phi 入边记真实 preheader（取消无消费者的魔法串 `"__entry"`）。
- `refresh_cfg`：`bb.preds`/`bb.succs` 改为由终结符单一重建，构建末尾与管线末尾各刷一次；
  `find_loop_body_multi` 沿 `preds` 反向走时不再读到旧边。
- SCCP：参与 phi 的名字与**多版本基名**一律不入常量表（否则 `s_0 = 0` 会传进循环体）。
- GVN：命中同形计算时，若 dest 基名是用户可见变量则**保留指令只改右式**（删它等于删赋值）；
  且只在改写真的记下时才允许删除（`register_rename` 返回 bool）。
- 调度与投机上提：补「基名序」约束（重建按基名塌缩成可变量，同基名的读/写顺序就是语义）。
- 结构不变式：`ssa::dangling_refs` + 单测 `loop_ir_pipeline_keeps_ssa_invariants`，
  直接跑 build→pipeline→rebuild，**与闸门无关**，因此生产默认下也能守住这轮修完的不变式。

### 4. 未关闭的已知缺陷（均有复现路径，不靠注释掩盖）

`make opt-eq` 今天以 4 个夹具报红起步，**全部是存量缺陷**（基线同样报出，本轮未新增任何一个）：

| 夹具 | 现象 | 基线对比 |
|---|---|---|
| `c_complex_abi.c` | -O2/-O3 与 -O0 行为不一致（`_Complex` ABI） | 基线同样 OPT-DIFF（存量） |
| `c_complex_div.c` | 同上（复数除法） | 基线同样 OPT-DIFF（存量） |
| `c_complex_literal.c` | 同上（复数字面量） | 基线同样 OPT-DIFF（存量） |
| `c_got_func.c` | 两个同名外部函数指针变量均被合并后，其中一个地址存成 32 位截断 | 基线在 -O2 **直接链接失败**（本轮修掉了初始化丢失与别名漏改写两处，剩这一处） |

另两项结构未关：

| 项 | 状态 |
|---|---|
| 循环闸门永久解除 | **已做完**（见 §6）：`func_has_loop` 及其四个专用判据已从 `opt.rs` 删除，不再需要 `DCC_SSA_LOOPS` |
| `ssa.rs::rebuild_cycle` | phi / 重命名 / SCCP / for 入口守卫 / 头块丢弃 / `continue`-`break` 分支保留均已修；余下缺陷见 §6 表 |
| P4/P13 与 P6–P9 的 hint | 对机器码零影响，未接消费者（删除会带走 PGO/并行化的已有测试，需单独一轮） |

### 6. 解锁完成：循环函数已永久进 SSA 管线（本轮）

`opt.rs::run_ssa_passes` 里的 `func_has_loop` 闸门连同 `block_has_loop` /
`stmt_has_loop` / `expr_has_loop` 四个判据已**删除**，含循环的函数现在无条件走
SSA 管线。为把这件事做到而不是「拔掉闸门留着错值」，本轮又修完下表的 9 类静默误编译：

| # | 缺陷 | 症状（实测） | 修复位置 |
|---|---|---|---|
| 1 | 重建期内联临时值不看基名序 | `while(n>0){s+=step*n; n--;}` 在 -O3 少算一圈（6 而非 20） | `ssa.rs::plan_inlined_temps` + `inline_window_safe`（同块窗口内不得写被读基名、不得跨内存改写、链式内联不得跨块）；被拒内联的 `_` 临时值同时进局部声明 |
| 2 | 被取址标量局部仍被 SSA 版本化 | `int acc=0;int*p=&acc;*p=5;return acc;` 在 -O2 得 0 而非 5（**与循环无关**，闸门也在时即错） | `Builder::is_mem_only_object` + `build_from_func` 两遍构建收集 `addr_taken`；读/写/自增/声明四处一律走内存路径 |
| 3 | `while (k-- > 0)` 的自减写在节点头却被丢弃 | 循环永不退出（忙等） | `rebuild_cycle` 的 `header_insts_droppable` + 不可丢弃时改发 `while(1){…; if(c){…}else{break;}}` |
| 4 | post-inc 直接返回旧版本名，塌缩后读在使用之后 | 同 3 的另一半 | `Builder` 的 `PostInc/PostDec` 分支先固化 `_tpost_N` |
| 5 | SSA 层 `loop_tiling`（P8）伪造 2x unroll：复制指令并复用同一 SSA dest | `for(i<n){s+=m*i;m=m+1;}` 在 -O3 得 38 而非 32（单赋值被破坏，重算发生在 `m` 改写之后） | **已删除** `loop_tiling`；同时删除 `loop_interchange`（只按块名互换 `block_order` 并写 `__dcc_loop_interchanged` 假 hint，不改 CFG/phi/边界） |
| 6 | `switch` 摊成 if-chain 后 `break` 改绑外层循环 | `for(i<3){switch(i){case 0:break;…}}` 在 -O2 得 0 而非 3 | `lower_switch_stmt`：链包进 `do{…}while(0)`；真 fallthrough / 绑定到 switch 的 `continue` / GNU 区间 case 一律**放弃降级**（函数留在 AST 路径） |
| 7 | -O3 展开只替换 `Expr`/`Return`/`Block` 里的归纳变量 | `for(i<4)for(j<4)s+=i*j;` 在 -O3 得 0 而非 36（副本读未赋值的 `i`） | `rewrite_stmt_ivars` 对 `Stmt` **穷举**（For/While/Do/If/Switch/…），并改为返回「是否覆盖完全」，覆盖不到即整条不展开 |
| 8 | 展开准入判据只看循环体直接条目 | `for(i<4){if(x)break;}` 被展开成 4 份悬空 `break`；`switch` 体内 `continue` 被误屏蔽 | `unroll_body_is_safe`：`break`/`continue` 分别按屏蔽位跟踪（switch 只屏蔽 break） |
| 9 | 调度器把循环体内不变量外提到循环前 | `while(n>0){int step=2; …}` 提出后 `step` 只在入口算一次，配合塌缩读错时机 | `speculative_execution` 的依赖闭包上提已有基名屏障；本轮补 `PreservedDecl` 进入内存定序/副作用链（见 §1） |

**度量**（均为本机实测，`JOBS=2`）：

- `make opt-eq`：OPT-DIFF **135（接入门禁时）→ 107（闸门仍在）→ 102（闸门删除后）**。
  解锁没有增加任何一项债，反而少了 5 项（循环类夹具转绿）。
- `cargo test --release -j2 --test-threads=2`：1924 lib + **991 integration**（新增 3 条）
  + 其余 target 全绿，exit 0。
- `check_fixtures`（595 个夹具全部被引用）/ `check_fixtures_selftest` 5/5 /
  `check_reproducible_output` 5040 组合字节一致，全部通过。

**新增常驻夹具**（逐条对应上表，全部经 gcc -O0 参照校正期望值）：
`ssa_addr_taken_local.c`（缺陷 2）、`ssa_rebuild_inline_order.c`（缺陷 1/3/4）、
`ssa_unroll_nested_iv.c`（缺陷 7/8），接进 `tests/integration/opt_ssa_gate.rs`。

**仍开放的存量误编译**（不靠注释掩盖，均有最小复现）：

| 项 | 最小复现 | 状态 |
|---|---|---|
| ~~4 层嵌套 + 变量边界 + `&&` 条件在 -O2 即错~~ | 同上 | **已修**（本轮）：根因不在形态也不在 LICM，而是 AST 层 GVN 的 `coalesce_block` 用**块内**写入集合判「一次性拷贝」——`for (int i = ti; i < 5; i++)` 的声明在 for-init 子块里，而写 `i` 的 `i++` 属于外层节点，于是声明被删、循环读从未赋值的 `i`。现在 `FnScope::assigned` 改成全函数视角；夹具 `gvn_loop_copied_iv.c`。v1–v6 探针与手写的 4 层分块形态现在 -O0/-O1/-O2/-O3 全一致。 |
| P10 分块产物进 SSA 后在 -O3 忙等 | `for(i<5)for(j<5)s=s+i*j;` 分块后 | **已修并已接入**（第 5 轮）：根因不是 `rebuild_cycle`，而是「`_` 前缀猜合成临时值」+「SSA 版本名与临时值 base 撞名」两个判据缺陷，详见 §7。转换本体由 `tile_nest_preserves_iteration_space`（小解释器逐点对比迭代序列）/ `tile_keeps_body_verbatim` / `tile_refuses_unsafe_nests` 三个单测锁住。 |
| `_Complex` / long double / 数学 builtin / percpu 等 102 项 OPT-DIFF | `make opt-eq` 名单 | 存量债，逐项消除中（本文 §4） |

### 7. 本轮（第 7～8 轮）：P16 的一次重大挫折与当前真实边界

**先写事实（重要，勿信下面的历史叙述）**：本轮末尾我做错了一次文件操作 —— 用
`sed -i "1,${L}d"` 想删掉未完成的「落地」段，`1,${L}d` 删的却是落地**之前**的全部
代码，即 P16 的循环识别、三段切分、实例计划、统一闸门与它们的单测全被删掉
（约 1596 行）。缓存与会话转录都救不回来（工具输出缓存只存 stdout，转录里的代码
已被压缩掉）。因此当前树里的 [src/ssa_pipelining.rs](file:///root/dcc/src/ssa_pipelining.rs)
**只有地基**：资源/延迟模型、依赖图、`res_mii`、`rec_mii`、`earliest_times`、
`edges_satisfied`、`modulo_schedule`，外加 7 个重建并全绿的单测。产物不受影响
（无人调用它）。（现状见下面 §7.1：识别/切分/闸门已重建，只剩落地。）

已按「测试即规格」重建的部分（`mod tests`，7 全绿）：

| 测试 | 固定的规格 |
|---|---|
| `no_carried_dependency_gives_ii_1` | 无携带依赖 ⇒ 无 distance>0 边、RecMII=1、II=1 可调度 |
| `carried_recurrence_gets_distance_one_edge` | `s = s + 1` 必须落成 distance=1 的**自环**，不得被去重误丢 |
| `high_latency_carried_loop_raises_mii` | RecMII 按闭合环 = 4（不是误算的 6）；II=1 被拒、II=4 可行且过 `edges_satisfied` |
| `res_mii_counts_load_slots` | 3 次 load ÷ 1 槽 = 3，÷ 2 槽向上取整 = 2 |
| `base_collapse_order_is_preserved` | 基名塌缩序双向建边（读→写、写→读），调度不得颠倒 |
| `memory_ops_serialize` | 内存操作两两定序 |
| `cyclic_same_iteration_is_refused` | 延迟和 > 距离和×II 的环必须拒绝该 II（II=1/2 拒、II=3 可行） |

同时删掉了一个**伪 P16**：`loop_opt.rs` 的 `software_pipeline_block` 只往循环前插一条
`Expr::AsmExpr{asm_str: "#pragma GCC pipelined"}`，该串在后端只是注释（实测 -O3 汇编
0 次出现），不产生 prologue/kernel/epilogue 任何变换；`volatile: true` 还反过来抑制
其它 pass 移动它。与上轮删除伪 `loop_tiling`/`loop_interchange` 同一标准，已连同调用点
与专用判据（`is_pipelineable`/`stmt_has_store`/`expr_has_store`）删除。

仍然成立的技术结论（不依赖已丢代码的部分）：**`rec_mii` 不能把不闭合的长链当环**。
用「到任意点的最长带距离路径」会把真值 3 的环算成 6，而它被用作 II 搜索起点 ⇒
直接**跳过本来可行的间隔**（抬高下界是丢收益，压低才是安全问题）。现在 `rec_mii`
只对闭合环求最大 Σlatency/Σdistance，并由上表第三条定住。该测试就是这条的回归。

已写下但**需重新验证**的结论：「真实语料上存在 `depth ≥ 1` 的跳迭代重叠」。它原本
由一个采样测试（`overlap_probe`）固定，该测试已随误删丢失，所以现在只能记为**待重新
实测**，不能作为 P16 收益已证实的依据。

P16 剩余工作（下一轮，按此顺序做，每步都带判定式自检）：

1. 循环识别：从 `Function` 取带 phi 的头、被头支配的唯一 latch、区域外唯一前驱
   （preheader），并确认区域是直线链（⇒不嵌套）。
2. 三段切分：以 `T(p,k)=stage[p]+k·II` 为唯一真源展开 `prologue / kernel×R / epilogue`，
   并断言它与 T 序**逐元素相等**且实例覆盖数 = 节点数 × 迭代数（这条自检历史上当场
   抓出两个方向性错误：kernel 迭代号没随轮次推进、跳轮引用偏移符号写反导致所有真实
   循环被误拒）。
3. 闸门只留一套：识别/trip count/线性归纳变量/区域大小/`trip ≥ 2*ii`/轮转级数全部走
   同一个入口，否则会出现「测试绿而实际跳过」；配套逐块拒绝原因报告。
4. 应用到 IR：节点重命名 + 三段块 + 一级轮转 phi，接入 `run_ssa_passes`，用执行夹具
   与 objdump 验收，并过 `-O0/-O2 结果一致` 与全部 Tier 0 门禁。

本轮收尾实测（删伪 pass + 重建地基后，均以 `JOBS=2 nice -n 10` 限 CPU）：

| 门禁 | 结果 |
|---|---|
| `cargo test --release -j2 -- --test-threads=2` | **全绿**（1935 lib + 993 integration + 其它套件），零 warning（本轮已推进到 1955，见 §7.1）|
| `scripts/check_fixtures.sh` | 597 个夹具全部被引用 |
| `scripts/check_fixtures_selftest.sh` | 5/5 PASS（门禁本身会被证明能红） |
| `scripts/check_reproducible_output.sh` | 5040 组合 × 3 次连跑字节一致 |
| `scripts/check_opt_equivalence.sh` | **40 条 OPT-DIFF（20 个文件 × -O2/-O3）**，少于上轮记录的 102 |

关于 102 → 40：本轮**没有**在删伪 pass 前后各跑一次做对照，因此不能把下降归因于某一项
改动（上轮那个数也没留下名单文件，无法逐项比对）。本表记录的是**当前权威值 40**；
剩下的 20 个文件就是 §4 那批存量债（`_Complex`、long double、数学 builtin、percpu、
预定义宏、rotate 内联等），仍为红，不得从宽判定。

### 7.1 第 9 轮：识别 / 切分 / 闸门已重建，只剩 IR 落地

上面那个表是提交时的状态（1935 lib）。本轮把§7 里「只剩地基」推进到
「只剩落地」：现在 `src/ssa_pipelining.rs` 共 22 条单测（地基 7 + 识别 10 + 切分/闸门 5），
并且识别与闸门均跑**真实 IR**（`parser → ir::lower → build_from_func`），不再只测合成图。
全量：1955 lib + 993 integration 全绿，零 warning。

**先用 dump 拿真实形状，再写分类器**（上一轮连错三次全是因为凭记忆假设形状）：

```
entry     : s_1 = 0, i_1 = 0
for.cond  : _tbin_1 = (i_1 < 8)   Branch t=for.body f=for.end   ← preheader，无 phi
for.body  : phis=[i_3, s_3]  idx=IndexLoad(a,i_3); s_2=_tidx+s_3   Jump for.step  ← 头
for.step  : _tpost=i_3+0; _tupd=_tpost+1; i_2=_tupd+0; _tpost=…; _tbin_6=(i_2<8)
                                                Branch t=for.body f=for.end  ← latch
```

由 dump 仍抓出两个真缺陷（都是先写错、被真实 IR 测试当场拦下）：

1. **拿「受头支配」当环内判据是错的** —— 循环出口同样受头支配（实测 `while.end` 的支配集
   含 `while.cond`）。两个后继都被当成环内后继 ⇒ 整个 `while` 形态被错拒。改用自然循环体
   集合（复用 `ssa::find_loop_body`，已 pub：另写一份只会与它漂移）。
2. **比较可能在头块、也可能在 preheader**：`for` 的 `i<8` 在 `for.cond`，`while` 的 `n>0` 在头
   自身。两边都有时不猜哪个控制回边，直接拒。

还有两个容易「测试绿但能力错」的点按事实处理了：

- 步长要穿透构建器的 `x = v + 0` 拷贝惯用法（`i++` 在 IR 里是 `_tpost=i_3+0;
  _tupd=_tpost+1; i_2=_tupd+0` 三行），只允许沿拷贝链遇到**恰好一次**真实步进；否则
  `i=i*2` 这类会被误收。
- 变量写在右边（`8 > i`）必须翻转运算符再判方向。测试同时钉住两面：`0 < i` 要当下降拒掉，
  `8 > i` 要当上升接受（只测前者等于「变量在右就拒」也能绿，那会默默丢掉一批可流水循环）。

**切分层的正确性不靠推导**：`InstancePlan::verify` 要求 `prologue ++ kernel×R ++ epilogue`
与绝对时间 `T(p,k)=stage[p]+k·II` 的序**逐元素相等**，且每个 `(节点,迭代)` 恰好出现一次。
三条负向对照（删一个实例 / 颠倒行内序 / 改坏 shift）全部必须被拒，否则「覆盖完整」是空判。

**收益判据拆成独立函数 `benefit_verdict`**：本来我拿「体内只写内存的循环」当 depth=0 的
反例，实测发现它照样通过闸门——因为 II 被**资源约束**抬到 3，stage 分布自然跨行。拿真实
IR 反推这条判据会得到错结论，所以改成用合成 (depth, ii, trip) 精确覆盖三条规则；真实 IR
那条只钉它该钉的事（分类器必须认得出这类循环，且一旦接受则计划自洽）。

**丢失的结论现在重新实测了**：`red`（`for(i<8) s+=a[i]`）的 RecMII=3、ResMII=3，在 ii=3 处
`stage=[0,2,0,1,2,1,3]` ⇒ **depth=1，真跳迭代重叠**；写死为
`shape_tests::recognized_loop_has_real_cross_iteration_overlap`。`plan_from_recognized_loop_is_sliceable`
进一步断言它的三段展开覆盖 7×8=56 个实例。P16 落地不再需要「先假定有重叠」。

仍差的只有一步：**把计划应用到 IR**（节点重命名 + prologue/kernel/epilogue 三段块 +
一级轮转 phi）、接入 `run_ssa_passes`、并用执行夹具与 objdump 验收。未接入前 `ssa_pipelining`
不参与编译产物，所以上表的 OPT-DIFF 与可复现结果与接入前同源。

### 7.2 第 10 轮：物化层——把「应用到 IR」从推导变成可判定的检查

新增 `materialize()`：把一个计划展成**一条线性指令序列**，每个 `(节点, 迭代)` 实例
复制一份，源操作数改写成「生产它的那个实例的输出名」（输名字统一成唯一的
`_pipe{seq}`），区域外操作数原样保留。它就是落地所需的全部名字解析规则。

**一个重要设计决策（走弯路后得到的）**：不要手工推 kernel 的轮转 phi 该携带哪个
版本的哪个变量。我尝试过按 `shift` 手推索引（节点 0 读 `i_3` 需要哪一轮的 `i_2`？），
这正是上一版实现反复出错的地方。现在的路径是：`resolve_operand` 已经把「该读
哪一个实例」算对了（56 个实例无一前向引用），kernel 的 phi 集合应当从这条规则**自动
生成**：对每条跨轮引用（`r' = r - 1`）生成一个 phi，latch 侧接生产者节点本轮的输出，
preheader 侧接 prologue 对应的实例输出；同轮前向引用直接接名字，不生成 phi。
`depth ≤ 1` 保证跳轮只能是 `r-1`，所以一级 phi 够用 —— 这不是凑巧，而是闸门判据的
直接推论。

准入加了一条 `inst_is_materializable`：区域内有 `Invoke` / `EhLandingPad` /
`PreservedDecl` / `InlineAsm` 时直接拒。理由逐条写在函数注释里（受保护调用复制多份
会把一个 call-site 区间撑成 N 个；用户声明复制就是重复定义；asm 的 `output_vars` 是
写侧名字，多实例共用会互相覆盖）。这些形态物化后不会产生编译错误，只会产生**错值**。

三条单测（`materialize_tests`）：

| 测试 | 内容 |
|---|---|
| `materialized_sequence_has_no_forward_reference` | `red` 的 56 个实例逐个检查：每个 `_pipe*` 引用都必须指向**更早**的实例，输名字全局唯一。这就是「模调度没有漏边」的直接证据 —— 漏一条边（基名塌缩序、内存定序、跳迭代）这里就炸。 |
| `materialize_rejects_reversed_row` | 负向对照：把 kernel 行内序颠倒，物化必须报错而不是产出前向引用序列。没这条，上一条可能是空判。 |
| `admission_refuses_unmaterializable_insts` | 体内含 `__asm__ volatile("nop")` 的循环必须在准入就被拒。 |

`rewrite_kind` 是**穷举 match**（`InstKind` 全部 16 个变体），新增变体时漏处理会直接编
不过，而不是静默漏掉一条依赖。不可物化的四个变体在 arm 里返回 `Err`（准入应已拦下，
万一漏过这里是第二道闸门）。

全量：1958 lib + 993 integration 全绿，零 warning。

### 8. 第 6 轮：P10 已接入 + P16 分析层完成

> 【校正】本小节写的是当时状态。其中 P16 表格的「准入」与「可观测」两行、以及表后
> 四个真缺陷对应的分类器代码，已随 §7 记录的误删丢失，目前**不在树里**；下表的
> 「依赖图 / MII / 调度」三行仍是现状（RecMII 现已改为只对闭合环求最大比例，见 §7）。
> 本小节的缺陷清单仍保留，因为它们对重建有效。

**P10 分块接入管线**。上轮挡住接入的那条查到了真因（见 §8 表第二项），本轮已把
`tile_block` 接进 `run_with_target` 的 -O3 序列。新单测三个（`tile_nest_preserves_iteration_space`
用一个小解释器逐点对比分块前后的迭代序列、`tile_keeps_body_verbatim`、`tile_refuses_unsafe_nests`）。
接入过程中又被两个独立缺陷卡住，均已修（见 §8）。

**P16 软件流水：分析与调度层已完成并被单测锁住；IR 落地待做。**新文件
[src/ssa_pipelining.rs](file:///root/dcc/src/ssa_pipelining.rs)：

| 组成 | 内容 |
|---|---|
| 依赖图 | 带「距离+延迟」的边：同迭代真依赖、**跳迭代真依赖**（由头块 phi 的 latch 侧输入回溯到生产者）、**基名塌缩序边**（同一基名的读→写/写→读/写→写）、内存定序边。自环（`s = s + 1`）只在 distance≥1 时保留——它正是递归本身，丢了等于抹掉串行瓶颈。 |
| MII | `ResMII`（各类操作数 ÷ 每行槽数）+ `RecMII`（Karp 式 DP：**同迭代边计延迟不计距离**，先做 distance=0 边的不动点闭包再用跳迭代边推层）。 |
| 调度 | epi-cyclic：解差分约束 `stage(v) ≥ stage(u) + lat - dist*ii` 的最小非负解（Bellman-Ford 最长路，可持续松弛 ⇒ 正环 ⇒ 该 ii 不可行），再按预留表做资源冲突顺延；最后 `edges_satisfied` 逐条边校验。**可行性判定不依赖任何启发式遍历顺序。** |
| 准入 | 方案的三条前置全部把关：单 latch 自然循环、区域是直线链（⇒不嵌套）、phi 只在头、**编译期已知 trip count** 且归纳变量是线性 `+1`、区域≤ 3 块 / ≤64 指令、`trip ≥ 2*ii`。 |
| 可观测 | `rejections(func)` 逐块报告「为什么没被选」。流水的准入是一串保守条件，任何一条误判都会默默让 pass 空转，而「它没跑」和「它为什么没跑」在产物上完全一样——本轮就是靠这个入口三次定位到分类器的错。 |

尚未做（下一轮）：把 `PipelinePlan` 落成 prologue/kernel/epilogue 三段块与寄存器轮转用的
phi，接入 `run_with_opts`，并补执行级夹具 + objdump 核对。**现在这个模块不参与编译，因此
不改变任何产物**（OPT-DIFF 仍 102、可复现门禁全绿已证明这点）。

本轮被测试抓出的四个真缺陷（都是我自己写的代码）：

1. 分类器把「循环头 = 分支块」当假设 → 真实 IR 里 `for` 的自然头是**带 phi 的
   `for.body`**，回边从 `for.step` 跳回 → 一个真循环也识别不到。改按支配关系找回边。
2. 归纳变量初值只查 preheader → `for.cond` 自己不含 `i = 0`（它在 `entry`）。
   改为「区域外唯一常量定义」。
3. `x = y + 0`（构建器的拷贝/物化惯用法）被当成步长 → 与真正的 `+1` 冲突，整条
   递推被误拒。现在只把非 0 常量计为步长。
4. `rec_mii` 第一版只累跳迭代边的延迟，把 `load(延迟2) → 下一轮消费者` 误算成
   RecMII=1 → 会去试一个根本装不下的间隔。现在环里的同迭代边也计延迟。

### 8-A. 本轮（第 5 轮）：两个新根因 + P10 分块已接入

上轮记为「P10 分块的前置：`rebuild_cycle` 的 while(1) 形式」的坑，本轮逐层查到真正的
两个根因（都不是 `rebuild_cycle`），修完并**已把分块接入管线**：

| 新发现 | 机制 | 症状（实测） |
|---|---|---|
| 重建用「base 以 `_` 开头」猜合成临时值 | `_s`/`__d` 是合法用户标识符，它们的赋值被内联掉 ⇒ 循环推进语句直接消失 | `int _s=0; for(i<n) _s+=i;` 与 `int __d…` 从 **-O2** 起算错（与循环闸门、与分块无关） |
| SSA 版本名与合成 base 共用 `_` 分隔 | `int _tbin` 的第 2 个版本名 `_tbin_2` 与 `fresh("bin")` 产生的临时值 base `_tbin_2` **完全同名** ⇒ 两个不相关的 SSA 名互相覆盖 | `int _tbin=3;for(i<4){s+=_tbin*i;_tbin+=1;}` -O2 得 18（应 39）；分块块索引 `__dcc_tile_i0` 同理被吞掉推进 ⇒ -O3 忙等 |

修法：
- `SsaVar::name()` 的版本分隔符从 `_` 改为 **`.`**（C/C++ 标识符不可能含 `.`，这类碰撞结构上消失；
  重建回 AST 的一律是 `base`，名字仅在 IR 内部存在）。相应更新 5 个写死了旧名字的单测期望。
- 新增单一入口 `ssa::is_synthetic_temp_base`：**用户可见声明集（`original_locals ∪ params`）优先**，
  其余只认已登记的合成命名方案（`_xxx` 与 `dcc_t<序号>_xxx`）。三个旧的前缀判定全改用它。
  中途一个收得过紧的版本误伤了 C++ 短路结果变量 `dcc_land1`（既不在 `original_locals` 也不属
  合成方案），已被 cpp ABI 交叉对照拓出（`ternary=` 第二项变 0）—— 该变量仍当具名局部处理。

新常驻夹具：[underscore_locals.c](file:///root/dcc/tests/fixtures/opt/underscore_locals.c)（下划线累加器/双下划线/
与内部临时名同形/与取址叠加/参数名），接进 `opt_ssa_gate.rs`。

回归（最终状态，全部本机实测）：`cargo test --release -j2 --test-threads=2` **exit 0**，
1928 lib + 993 integration；OPT-DIFF **仍 102**（本轮零新增）；`check_fixtures` /
`check_fixtures_selftest` / `check_reproducible_output`（5040 组合字节一致）全绿；
14 个探针（含分块、向量、嵌套、下划线名）-O0/-O1/-O2/-O3 与 gcc 逐字一致。

### 8-B. 本轮（第 4 轮）补记

- 修完上表那条 -O2 GVN 误编译（它同时是解锁期间的一个独立存量错值，不依赖循环闸门）。
- P10 分块从「伪实现（静默错值）」重写为「形态正确 + 三个结构单测」，并在第 5 轮接入（当时
  挡住接入的那条已查到底，见 §7）。旧 `generate_tiled_loop` 的三个缺陷（名字空间碰撞、
  `subst_stmt` 漏 `Decl` 分支、单循环分块无收益）已写进 `loop_opt.rs` 的 P10 一节，带实测数值。
- 回归：1928 lib（+3）+ 992 integration（+1）+ 其余 target 全绿；OPT-DIFF 仍 102；
  `check_fixtures` / `check_fixtures_selftest` 5/5 / `check_reproducible_output` 5040 全绿。
- 下一轮目标不变：P16 软件流水本体（modulo scheduling）与 P12 寄存器分配（需后端级重写）。

### 5. 新门禁

- `scripts/check_opt_equivalence.sh`（`make opt-eq`，并接入 `make test-all`）：
  同一源码 -O0 / -O2 / -O3 的 stdout 与退出码必须逐字节一致，OPT-DIFF 非空即 exit 1。
  **接入时即为 4 项红**（全部存量）—— 它的价值是把这类缺陷从「没人知道它存在」
  变成「必须逐个消除的债」，不得为了上绿而从宽判定或下调名单。
- `tests/common/harness.rs`：`run_exe_bounded` 统一给所有跑产物的测试加超时（默认 20s，
  `DCC_TEST_EXE_TIMEOUT_SECS` 可改）—— 死循环误编译不再是「吊死 cargo test」而是「跑红」。
- `run_gcc_cross_check_opt`：带 `-O` 档位的 gcc 交叉对照（旧 `run_gcc_cross_check` 零参数 = -O0，
  这是「优化夹具其实只测了 -O0」的根源）。
- 新夹具：`licm_*.c`（6 个，逐条对应 A 的根因）、`ssa_gate_switch_loop.c`（对应 I）、
  以及 §6 里的 `ssa_addr_taken_local.c` / `ssa_rebuild_inline_order.c` /
  `ssa_unroll_nested_iv.c`（逐条对应本轮修完的 9 类误编译）。
  新测试文件 `tests/integration/opt_licm.rs`、`tests/integration/opt_ssa_gate.rs`。
- SSA 结构不变式（`ssa.rs`）：`dangling_refs`（不得有悬空引用）与
  `duplicate_ssa_defs`（**全函数单赋值**）。后者配一个**负向对照**测试
  （`duplicate_ssa_defs_detects_appended_copy`：把一条指令原样追加一遍，检查器必须报错），
  否则不变式可能是空判。这条不变式正是被删的伪 `loop_tiling` 破坏的那一个。

