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
| P16 | software_pipelining | ⚠ stub | 空函数体却 `modified=true`；被循环闸门结构性锁死 | ✅ **已真实现并默认接入 -O2/-O3**（第 17 轮，见 §7.6） |
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
| **度量修正**：OPT-DIFF 真实是 **102 条 / 51 个文件**（COMPILE-FAIL 63，语料 335）| `make opt-eq` 的 `summary.txt` | 本文此前记的「102」与上一轮记的「40」都不是名单总量：门禁脚本在**显示**处带 `| head -40`，而统计用的是 `grep -c '^OPT-DIFF'` 对 **stdout**，于是把显示截断当成了总量（上一轮真正的值是 40 条显示 = 前 40 行）。已去掉截断，现在打印 `TOTAL OPT-DIFF=<n> 条 / <m> 个文件` 并列出全部条目，`grep -c` 与 `TOTAL` 从此自洽（本轮实测 102/51；并行审计会话测得 103/52，差的 1 条是两次运行间的名单漂移）。存量债逐项消除中。 |
| ~~新：P16 落地的 store kernel 缺陷（因此默认不接入产物）~~ **已关闭（第 17 轮）** | 最小复现：`static int arr[12]; int main(void){ for(int i=0;i<12;i++) arr[i]=i; printf("done=%d\n",arr[5]); }` | 实测三档与 gcc 一致（`done=5`），且该循环现在被准入拒收（`depth=0`）。真正的根因不是 `IndexStore` 专属，而是「基名塌缩后的反依赖未被判定覆盖」——同一条边同时解释越界写与本节的错值。修复与度量见 §7.6。 |
| ~~新：同一翻译单元内的跨函数污染~~ **已关（第 20 轮复测）** | `tests/fixtures/opt/ssa_pipe_nested.c` 的 `nest_three` 形态：函数内 `static int buf[6]` + 三层循环 `buf[j] += i + k` | 当年实测 -O2/-O3 得 210（应 36），且单函数独立成 TU 是对的 ⇒ 跨函数/TU 级污染。本轮按原形态重建（函数内 static 数组 + 两个无关旁函数）复测：`-O0/-O1/-O2/-O3` 全部 36 且与 gcc 一致 ⇒ 随本轮 SCCP/往返类修复一并消失。夹具里的这条形态留着继续钉（它回归过一次）。 |

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

### 7.3 第 11 轮：P16 落地完成，但默认不接入产物（两条实测缺陷）

本轮把计划真正应用到了 IR，并同时拿到两条“必须默认关闭”的实测证据。

**为何 kernel 必须是单块**：真实 kernel 行的节点在原 IR 里分属 `for.body` 与
`for.step` 且**跳块交错**（`red` 的行序是 `[0,2,6,3,5,1,4]`），两块形状表达不出这个
序，而序就是正确性（`edges_satisfied` 保证的正是按 T 序串行执行）。所以 kernel 落成
一个带 phi、带条件回边的**单块自然循环**（与 `while.cond` 同形）：prologue 追加到
preheader、epilogue 前置到出口块、latch 块核实无人引用后摘除。

**轮转 phi 从规则自动生成，不手推 shift**（上一版就在此连错两次）：
`back = shift[q] - shift[p] + (携带?1:0)`；back=0 是本轮前向、back=1 才建 phi、其它拒。
本轮这条公式又错了两次，**两次都被前向引用检查拦下**：先是只把 phi dest 当跳轮引用
（漏了`节点 6 读 i_2` 其实跳上一轮），后是把两个 shift 写反（back 算成 -1）。

**失败安全**：落地全部在 `Function` 副本上进行，只有 `dangling_refs` 与 `duplicate_ssa_defs`
都通过才替换原函数。两条测试分别钉住「拿旧计划改新 IR 必须被拒」与「中途失败时原 IR
逐字不变」。

**区域外的引用必须一起重写**（第一次实现漏了这整类）：出口块的 `return s` 读的就是
区域内定义的值，不改写就留下 `for.end: s.2` 悬空；而且 **terminator 里的操作数也算**
（Ret/Branch/SwitchTable 穷举重写）。

**接入默认关闭的两条实测理由**（详见 §4 表）：
1. `IndexStore` 被流水后**越界写**：`for(i<12) arr[i]=i` 在 -O2 下输出整块丢失而退出码仍为 0。
   隔离实验证明「归约在被调函数内」与「归约不读内存」两种形态都正常，所以病灶锁定
   在 store 实例的写地址迭代号。
2. **嵌套场景的两类错**：外层携带值被当循环不变量（累加丢失），以及落地重写了外层
   赖以前进的块（产物死循环，`for(i<10) for(j<10) sum+=i*j` 实测挂死）。因此新增准入
   `is_nested_in_other_loop`：嵌在其它循环里的循环一律不流水（两层都被拒，由
   `nested_loops_are_refused_on_both_levels` 钉住）。这保住了单层循环（方案的典型受益形态）
   的可流水能力，而不把死循环送进产物。

新入库两条执行级守门夹具 `ssa_pipe_reduction.c` / `ssa_pipe_nested.c`：它们在**默认路径**
上是绿的（证明未接入时产物未被污染），而一旦把流水默认打开就会立刻变红 —— 下次接入
不需要重新发明验收。

**本轮同时推翻了两个我此前写下的结论**（均按证据改正）：
- OPT-DIFF 不是 40 条而是 **102 条 / 51 个文件**：门禁在显示处 `| head -40` 截断，而我用
  `grep -c` 统计 stdout ⇒ 把截断当成了总量。脚本已修（打印 TOTAL 并列全部条目）。
- P12 旧论据「三后端只有栈槽、什么也没有」在 x86_64 上**不成立**：已有 `reg_bindings`（变量
  整体驻留物理寄存器 + 栈槽回退，由 `register asm` 驱动且已测试）与一个 4 寄存器临时池。
  所以 P12 的**实质价值可增量落地**（把分配结果灌进现成的 `reg_bindings`/`VarDecl::register_name`
  通道，不动 `ast::Ident`、不动 `ir.rs`、不动其它后端）；而文档要求的**字面形态**（`Ident` 携带
  寄存器号 + codegen 不再分配 + 真 SPILL/RELOAD）受三条硬约束限制：单遍直发字节、`frame_size`
  在函数体之前就定死、回填只能等长 ⇒ 需重写发射模型。其中「取址与 `"m"` 约束不查
  `reg_bindings`」「规划期 `pre_register_vars_max` 无脑分槽」两项本身就是现存 bug，可零风险先行。

另外发现两个**门禁自身的缺陷**（它们会让“已修”变成自欺）：`golden_baseline/hashes.txt` 与各
脚本零关联（无任何消费点，是个静死工件）；`arm_mips_golden_baseline.sh` 里三个夹具路径全部
MISSING，实为空跑。

### 7.4 两份专家审计后的待开工项（按收益/成本排序）

| 序 | 动作 | 位置 | 预期效果 |
|---|---|---|---|
| 1 | ~~不可降级表达式改为「整函数放弃 SSA 化」~~ **已做（第 18 轮）**：`Builder::unsupported` + catch-all，整个函数退回 AST 通道；顺手修了同一层的两个静默误编译（逗号丢左侧、三元 `Select` 无条件执行两侧） | `src/ssa.rs::Builder::{unsupported,cur_inst_count,emitted_is_pure}` | **OPT-DIFF 102 → 32**（实测） |
| 2 | ~~一元运算重建取真实类型~~ **已做（第 18 轮）**：真正的错点不在 `inst_to_expr`（那已是旧状态）而在**唯一有人调用的** `inst_to_expr_resolved`，它只认向量类型其余写 `Type::Int` ⇒ 浮点取负在 -O2 丢失。现由 `unary_result_ty` 以 `dest` 类型为真源 | `src/ssa.rs` | 修掉「`double n = -b` 得 0」这类错值；math builtin 系列 OPT-DIFF 消失 |
| 3 | ~~`reg_bindings` 闭合：取址与 `"m"` 约束查绑定表~~ **已做（第 19 轮，见 §7.8）**：改成「逸出即不绑定」而非「读侧查表」，逸出判据复用 `ssa_gvn::addr_escaped_names`；「规划期尊重绑定」**故意保留不变**（栈槽是逸出的退回路径，必须始终存在） | `src/ssa_gvn.rs`、`emit/functions.rs`、`emit/stmt.rs` | 实测从错值 `42 0` 变正确的 `42 42`；并补上了逸出收集对语句表达式/复合字面量/asm 操作数的漏报 |
| 4 | ~~修 `IndexStore` 的 kernel 实例~~ **已做（第 17 轮）**：根因是基名塌缩反依赖，由 `refine_kernel_row` + `emitted_reads_match` 闭掉，流水已默认接入 | `src/ssa_pipelining.rs` | 见 §7.6 |
| 5 | ~~clrsb 位宽取形参类型 + 改夹具错值期望~~ **已做（第 18 轮）**：位宽改取 `Type::Long`/`LongLong`，夹具期望按 gcc 实测改为 31 | `special/builtins.rs`、`tests/fixtures/c/rotate.c` | 与 gcc 真值对齐（三档与 -O0 均一致） |
| 6 | `DCC_OPT_SKIP=<pass>` **已做（第 18 轮）**：覆盖 AST 层全部 10 个调用点，与 `DCC_SSA_OFF` 合起来能二分管线任意一段；跨函数污染（`nest_three`）**也已关**（第 20 轮按原形态复测四档均为 36 且与 gcc 一致） | `src/opt.rs::opt_skipped` | 本轮就是靠它把剩下 18 条 OPT-DIFF 全部归因到具体 pass（见 §7.7 表） |

目标数值路径：102 → (1) 32 → (2)(5)+门禁归一化 18（/9 个文件）。(4) 已于第 17 轮
完成且 P16 已默认接入；剩下的 18 条已逐条归因，见 §7.7。

### 7.5 第 12 轮：两个推翻性的实测——条件节点不能进调度区，接入点必须在管线最前

本轮把落地真的跑进了产物（`DCC_PIPE_ON=1`），结论推翻了上面两个记录。

**一、`depth=1` 是假的，原因是判定指令被算进了调度区。**

上一轮声称在 `red`（load 归约）上实测到跳迭代重叠（`stage=[0,2,0,1,2,1,3]` ⇒ depth=1）。
其中那个 stage=3 就是 latch 的 `_tbin_6`：它不是计算，而是循环控制。剔除后最大
stage=2 而 II=3 ⇒ **depth=0**，闸门如实拒收。本轮把条件节点从调度区里拿了出来
（`PipelinedLoop::cond`），它必须单独处理，两个理由都是实测的：

1. 它读的是**本轮更新后**的归纳变量（即下一轮的值）。把它当普通节点放进 kernel 行，
   这个读会被解析成轮转 phi（本轮进入值），判定提前一轮 ⇒ 多跑一轮。对写数组的循环
   就是 `a[trip]` **越界写**（上一轮那个“-O2 输出整块丢失而退出码仍为 0”就是它）。
2. 计入调度区会虚构重叠，把“不值得流水”算成“值得”。

同时修正两处形态假设：判定指令的位置随循环形态不同（`for` 在 latch、`while` 在头块
自己），只查 latch 会把 `while` 误拒为“latch 不是条件分支”；preheader **必须保留条件
分支**，不能改成无条件 Jump，否则首次进 kernel 无人判定，整个循环变成 do-while 语义、
至少多跑一轮。kernel 因此在全部节点之后再推一份判定实例（`apply_produces_valid_rebuildable_ir`
钉住 `insts == nodes + 1` 与 preheader 仍是 Branch）。

**二、接入点放在管线末尾，一次也不会流水。**

把 `run_pipelining` 留在 `run_ssa_passes` 尾部时，`DCC_PIPE_ON` 开/关的 -O2 汇编 **sha256 完全
相同**（产物逐字节一致）：ivop / GVN / SCCP / 展开 / 分块早就把归纳变量链与 latch 改成了
分类器不认识的形状。现在把它移到 `run_with_opts` 开头（`level >= 2` 之后、其它 pass 之前），
产物出现 184 行差异 ⇒ 流水真的发生了。教训是：**“pass 已接入”必须由产物差异证明，不能由
代码已调用证明** —— 否则它就是一个永远不生效的默认参数，而单测全绿完全看不出来。

**三、于是拿到了第一个真实的端到端错值（当前的阻塞项）。**

同一依赖链样本（`tests/fixtures/opt/ssa_pipe_chain.c`，含“该流水”的 `ch`/`ch_store` 与
“不该流水”的 `red`）在 `DCC_PIPE_ON=1` 下：

```
-O2: ch=8        期望 ch=475027   （-O3 与 gcc 一致）
```

即在真实管线里 kernel 的轮数/phi 初值仍不对，而且只在 -O2 出现（-O3 因为另有 pass
顺序而遮住了它，说明这个 bug 对顺序敏感）。IR 层的 34 条单测抓不到它，因为它们是在
未经其它 pass 的 SSA 上 classify 的 —— 下一步止步就是在“管线中途”的形状上做落地前后的
等价校验（或把 `ssa_pipe_chain` 的 kernel 轮数直接断言出来）。**在修好之前流水不得默认接入。**

#### 第 16 轮更新：临界点已缩小到「区域节点数 ≥ 6」

上一段里的 `ch=8` 已被 `2f3a06a`/`374fb7a` 推进，但**错值并未消失**。用参数链
`v1+=1; v2+=v1; …; vm+=v(m-1)` 在 `DCC_PIPE_ON=1` 下对 gcc 逐个实测：

| 规模 | -O2 关闭 | -O2 开启 | gcc 期望 | 结论 |
|---|---|---|---|---|
| m=2..5，t=6/24 | 正 | 正 | 同 | 全部 OK |
| m=6, t=6 | 462 | 462 | 462 | OK（被 prologue/rounds 组合掩盖）|
| m=6, t=10 | 5005 | **7006** | 5005 | 错 |
| m=6, t=14…24 | 正 | **全错** | 同 | 错值都比期望多≈`v5(t)`（如 t=24 多 98279，而 `v5(24)=98280`）|

三个结论：

1. **与 trip 无关，与区域节点数有关**：m=6（区域内节点更多）才触发，m≤5 全对。
2. **错的是「最后一个节点多累加了一整轮」**：多出的数值恰好等于上一级变量 `v5` 的终值。
   即 `prologue/epilogue` 的覆盖边界在节点数变大时错位（不是回边、不是 phi 初值）。
3. **默认关闭下全部正确** ⇒ 不会进产物；但流水仍不得默认接入，直到该边界修对。

下一轮起点（比重新猜便宜得多的两个工具）：把上面 m=6/t=10 这个**最小错值形态**写成
IR 层断言（算一遍 `prologue ++ kernel×R ++ epilogue` 对每个节点的**实际执行次数**，
应该每个节点都恰为 trip 次），以及把校验从「classify 原始 SSA」改成「classify 管线中
途的 IR」——现有 34 条单测两者都没覆盖，所以它们全绿而这个 bug 存在。

默认路径（不开 `DCC_PIPE_ON`）下 1962 lib + 996 integration 全绿，`ssa_pipe_chain` / `ssa_pipe_reduction` /
`ssa_pipe_nested` 三条执行夹具逐档与 gcc 一致。

### 7.6 第 17 轮：P16 的错值根因是基名塌缩反依赖，已修并默认接入

上一轮把临界点定在「区域节点数 ≥ 6」与「最后一个节点多累加了一整轮」。本轮按
§7.5 留下的两个工具入口查，结论是：**那不是 prologue/epilogue 边界错，也不是
节点数阈值，而是发射序缺了一道判定**。

**根因（实测到指令粒度）**：SSA→AST 重建把同基名的 `e.4`/`e.5`/`e.14` 全部还原成
同一个变量（同一个栈槽），head 块上那对轮转 phi 因此折叠成 `e = e` 被丢弃。于是
「读上一轮的 e」在机器码里**不是**读一个轮转寄存器，而是读那个槽的当前内容 ——
它等于哪份实例完全取决于发射顺序。chain6 的 kernel 行按 `stage%ii` 把 `e += d`
排在 `f += e` 之前，所以 `f` 每轮读到本轮的 e：少加第一轮 e（=1）、多算最后一轮 ⇒
`f = 5004 + 2002 = 7006`（真值 5005）。模调度的 T 序模型只给真数据依赖定序，而这
是一条**反依赖**（WAR），模型里根本没有这条边 —— 所以 34 条 IR 单测与 `verify` 全绿
依旧错值。m≤5 之所以「全对」，实测是因为它们 `depth=0` 被闸门拒了、根本没流水（见下）。

**修的方式（按「不靠推导，靠判定」）**：

| 改动 | 作用 |
|---|---|
| `InstancePlan::row_pos` / `key` / `with_row` | 行内序提升为一等公民：`verify` 的平局尺子改为行内位置（而不是 `stage%ii`），并要求各行一致、是节点集的排列。平局意味着图上无约束 ⇒ 发射序就是唯一真源。 |
| `refine_kernel_row` | 按行内约束稳定重排平局：本轮前向 ⇒ 生产者先，轮转读 ⇒ 消费者先（累加器自读自己天然满足）。Kahn + 原行序优先 ⇒ 确定性且能不动就不动；约束成环就拒（基名塌缩下确实无解，需真轮转寄存器）。 |
| `emitted_reads_match` | **发射层的最终闸门**：沿 `prologue → 每轮「head 判定 + kernel 行」 → epilogue` 模拟槽位内容，要求每个读操作数都命中模型指定的那个实例（携带值 = 上一迭代，区域内定义 = 同迭代，区域外 = 仍为循环前的值）。漏任何一条边都以拒收结束，不会以错值进产物。 |
| `back_rounds(shift_p, shift_q, carried)` | 跳轮公式收敛为单一真源，落地与行序约束共用；判定指令不再伪造成「节点 0」，而是显式 `COND_SHIFT`，顺手消掉一个潜伏错（旧代码里 `init_iter(q, 0, …)` 取的是 `shift[0]`，恰好不致错才没暴露）。 |
| `exec_counts` | 每节点实际执行次数必须恰为 trip（§7.5 要求的那条断言），带反向对照。 |
| `DCC_PIPE_DUMP=1` | 诊断窗口：打印识别结果/依赖信息/stage/三段切分/落地后 IR，**并打印每个候选头的拒因**。没这个窗口就只能猜形状，本模块多轮修复反复卡在这点上。 |

**接入与度量（均为本机实测）**：

- 默认接入：`run_with_opts` 里去掉 `DCC_PIPE_ON` 门槛，`DCC_SSA_OFF=pipe` 仍可关掉二分。
- 「真的改了产物」由新集成测试 `ssa_pipe_default_on_changes_the_product` 钉住：同一份
  夹具同一档位，只切 `DCC_SSA_OFF=pipe`，-O2 汇编必须不同（§7.5 的教训：接入与否
  不能看代码是否被调用）。
- 全量：`cargo test --release -j2 -- --test-threads=2` **exit 0**，1965 lib + 997 integration
  全绿，零 warning（新增 3 条 IR 层单测 + 1 条集成单测）。
- OPT 等价门禁：`scripts/check_opt_equivalence.sh` 仍为 **102 OPT-DIFF / 63 COMPILE-FAIL**
  与本轮前完全一致 ⇒ 默认接入在整个夹具语料上零新增回归。
- 覆盖面：前 400 个夹具文件里共 3 个循环被流水（`ch`、`chain6_10`、`red8_scaled`）。
  其中 `red8_scaled` 是 load 乘 2 归约（trip=8、depth=1），它的结果被 printf 打印并与
  gcc 逐字节对照 ⇒ 首次拿到「真流水的循环跳档与 gcc 一致」的执行级证据。

**同时推翻/修正三条旧结论**：

1. §7.3 把默认关闭的理由之一定为「`IndexStore` 被流水后越界写」，具体到「kernel 内
   store 实例的写地址迭代号错位」。本轮实测：当前 `ch_store` 在准入处就被拒
   （「归纳变量初值不是区域外的唯一常量定义」），store 形态根本不进流水；而越界写
   与本轮这条错值同源于基名塌缩反依赖（当时多跑一轮 ⇒ 写了 `a[trip]`）。所以旧归因
   方向对（写地址迭代号）但定位不准（不是 store 专属 bug）。
2. 「m≤5 全对」不是「流水对了」，而是 `depth=0` 被拒。实测：5 级链最后一个节点
   stage=4 而 II=5 ⇒ depth=0；6 级链才出现 stage=9 的跳行。所以本仓语料里唯一
   能流水的形状就是 6 级依赖链（以及带乘法的 load 归约）。
3. §7.4 表里的「唯一有重叠的 depth=2」已不存在：`benefit_verdict` 一直拒 `depth>1`，
   所以所有被接受的计划 depth 恒为 1（由 `dependency_chain_yields_real_overlap_and_is_accepted`
   钉住）。

仍未做（诚实记录）：真轮转寄存器（同基名难同时保留两份）——拿不到合法行序的循环现在
被**拒收**而不是错跑，代价是丢收益；`DCC_PIPE_DUMP` 会把它们的拒因列出来。

### 7.7 第 18 轮：OPT-DIFF 102 → 18，剩下的逐条已归因到具体 pass

本轮先把 §7.4 里成本/收益比最好的几项做完，再把剩下的存量债**逐条定位**（上一轮只会
写「102 条待消除」，连哪条属于哪个 pass 都不知道）。

**做完的**（每项都有实测数字，不是「应该修好了」）：

1. **不可降级表达式不再静默变 0**（§7.4 项 1）。`lower_expr_inner` 的 catch-all 与
   `&下标`/`&成员` 两个口子现在都会设 `Builder::unsupported(原因)`，`build_ssa` 见到它
   就**放弃整个函数的 SSA 化**（该函数留在 AST 通道，`opt.rs` 本来就是 `if let Some(ssa)`）。
   新窗口 `DCC_SSA_BAIL=1` 打印是哪个函数因为什么不进管线。
   ⇒ **OPT-DIFF 从 102 降到 32**（51 → 16 个文件），全量测试绿。
2. **额外发现两个同类静默误编译**（都不在 §7.4 清单里，是同一根「降级层默默丢语义」）：
   * `Expr::Comma { l: _, .. }` 直接**丢弃左侧** ⇒ `x = (f(), 1)` 里的调用在产物里消失。
     现在两侧都照常降级。
   * `Expr::Cond` 无条件降级成 `InstKind::Select`，而 Select 是「两侧都算完再选」⇒
     `x < y ? x++ : y++` 两边自增都执行（实测夹具直接 abort）。现在发射序里出现任何
     存/载/调用/可陷获运算/给用户变量赋值都整个函数退回 AST 层。判据建在**实际发射出的
     指令**上而不是 AST 白名单上，所以不会与降级代码漂移（`emitted_is_pure`）。
   ⇒ 32 → 26。
3. **一元运算重建取真实类型**（§7.4 项 2）。方案里的行号指向 `inst_to_expr`，但那个函数
   **没有任何调用点**（当时已改成读操作数类型了），真正跑的是 `inst_to_expr_resolved`，
   它仍是「只认向量类型，其余一律 `Type::Int`」。后果不是数学 builtin 而是**任意浮点取负**：
   `double b = 2.3; double n = -b;` 在 -O2 得到 `0.000000`（重建后 `Unary{ty:Int}` ⇒
   codegen 的 xorpd 分支落回整数 `neg r64`）。现在类型以指令 `dest` 为真源（`unary_result_ty`），
   并对无 dest 的情形按 op 推（`!x` 永远是 int）。⇒ 26 → 22。
4. **clrsb 位宽取形参类型**（§7.4 项 5）。旧码用 `args[0].ty().size()`，而
   `__builtin_clrsbl(0x80000000)` 的字面量是 `unsigned int` ⇒ 错算成 32 位宽。现在按
   `Type::Int`/`Long`/`LongLong` 定型 ⇒ 与 gcc 一致（31）。同时发现**夹具自身期望是错的**
   （写的是 0），已按 gcc 实测改正——这条属于「错值期望把错实现钉住了」的典型形态。
5. **`DCC_OPT_SKIP=<pass>`**（§7.4 项 6 的前半）：覆盖 AST 层 10 个调用点。本轮剩余
   存量债的归因全部出自它 + 原有 `DCC_SSA_OFF`（两者合起来才能二分整条管线）。
6. **`__NO_INLINE__` 与 `__OPTIMIZE__` 不再同时成立**（新发现，不是优化器 bug）：
   dcc 在所有档位都定义 `__NO_INLINE__`，gcc 只在 -O0 定义。修时发现它是**影子副本**致：
   `preprocess()` 里两处各定一次，后者无条件盖掉前者的档位判据。现在只留一处。
   另外 `tests/fixtures/c/predefined_macros.c` 本身要求「`__OPTIMIZE__` 与 `__NO_INLINE__`
   都已定义」，而这两个宏按定义不可能同时成立 ⇒ 夹具自相矛盾，已改成只断言「恰好一个」。
7. **OPT 门禁不再把 ASLR 报成 bug**：`check_opt_equivalence.sh` 直接比 stdout，而夹具
   `attr_returns_nonnull.c` 打印堆地址 ⇒ 每次运行都不同，永远 DIFF。现在比较前先抹长
   十六进制串，并且**被抹掉时记一行 `ADDR-NORM`**，不让归一化变成静默放过。
8. **`arm_mips_golden_baseline.sh` 从空跑变成真门禁**（§7.3 登记的第二个门禁缺陷）。实测
   四个叠加问题：三个夹具路径不存在且 `continue` 跳过；目标开关写成 `-m=arm`（实际是
   `--target <triple>`）；`normalize_asm` 在定义之前就被调用；基线每次重新生成而不比较。
   现在：夹具缺失/编译失败/产物漂移一律置红，一个都没跑成也算失败；基线改存
   `tests/golden/arm_mips/`（原 `golden_baseline/` **整目录被 .gitignore 排除且零文件入库**，
   存在那里的基线永远不可能成为别人仓库里的门禁），并以 `make golden-cross`（`verify` 模式）
   接入 `make test-all`。已实测捕获幂等与 DRIFT 能真被拒出。

**剩下的 18 条 / 9 个文件，已逐条归因**（每行都是新开关实测，不是猜测）：

| 文件 | -O2 现象 | 关掉谁就正常 | 下一步 |
|---|---|---|---|
| ~~`va_arg/global_valist_sse_agg.c`、`va_arg/vacopy_sse_agg.c`、`va_arg/mixed_sse_pipeline_named.c`、`va_arg/mixed_sse_via_valist_param.c`~~ **已修（第 20 轮，见 §7.9）** | 退出码 = 首个读错位置的序号（15/10/20/13） | **只有 SCCP**（`DCC_SSA_OFF=sccp` 就全对） | 归因对了但病灶更早：SCCP 的常量表**不带类型**，于是把 `Type::Struct`（va_list 聚合）的槽位当 int 常量替换。已按操作数类型类守卫，四个夹具四档与 gcc 一致 |
| `vla.c` | rc=12 | **只有 SSA 层 GVN**（`DCC_SSA_OFF=gvn` 就全对） | 未修；两种候选方案已被实测否定，证据与下一步见 §7.10 |
| ~~`complex_abi.c`~~ **已修（第 22 轮，见 §7.13）**；~~`agg_param_reg_stack_split.c`~~ **已修（第 23 轮，见 §7.14）**；`ld80_arith.c` | 复数：rc=1；聚合：rc=6 | 这两条都不在优化 pass 里，而在**降级/重建的形状丢失**上 | 只剩 `ld80_arith.c`：80 位 long double 过 SSA 往返丢精度，属 `docs/longdouble_80bit_migration.md` 的未完部分。它与仍未修的 `vla.c` 一起指向同一个结构性缺口：**值种类在 IR 里靠 `Type` 猜，而不是各自有形态**（§7.14） |
| ~~`chained_assign_truncation.c`~~ **已修（第 21 轮，见 §7.11）** | abort | `DCC_SSA_OFF=sccp` 就正常，但根因不在 SCCP：是构建器丢了中间类型 + 全局赋值返回了原始 RHS | 见 §7.11 |

**§7.4 项 3（`reg_bindings` 闭合）本轮没做，理由不是时间不够**（下一轮已做完，见 §7.8）：
查下去发现它不是一个
「补一句查表」的改动。当前只有 `mov_from_stack`/`mov_to_stack` 两处看绑定表；要闭合就必须
先回答「这个变量的地址逸出了吗」，而这需要（a）一个覆盖全函数表达式的逸出收集（包括
asm `"m"`/`"=&m"` 约束的操数，它与 `&x` 是同一个事实但 AST 里没有 `AddrOf`），
（b）跳函数可见性（文件作 `register asm` 变量的地址可在任何一个函数里被取），
（c）新的活性夹具（现在 `register asm` 的夹具全在 `tests/fixtures/_archive/dev/`，
即活性套件对这一特例 **零覆盖**）。(a) 已有可复用的完备实现（`ssa_gvn::walk_addr_of`），
但它得先补齐 `AsmExpr`/`StmtExpr`/`CompoundLit` 等遗漏变体（补了会同时收紧 GVN 的逸出集，
方向安全但需重跑全量）。这每一项都比「改两行」重，所以本轮没有拿它换表面上的绿。

### 7.8 第 19 轮：`register asm` 与地址逸出的矛盾（§7.4 项 3）

上一轮说这项不是「补一句查表」，本轮把理由逐条做完并验证。

**实测到的错值**（不是推演）：

```c
register long v asm("r15") = 0;
long *p = &v;
v = 42;
```

修复前（`191fa23` 的编译器）：打印 `42 0`、退出 1 —— 绑定生效后 `v = 42` 只写 `%r15`，
那个栈槽**从来没人写过**，`*p` 读到的是栈残留。修复后四档均 `42 42`、退出 0。
（gcc 对这种写法直接拒绝编译：`error: address of register variable 'v' requested`，
所以这份夹具只能断言 dcc 自己的语义，不能做 gcc 对照。）

**判据的位置：不新增一套，而是复用已有的那一套**。方案原本写的是「读侧查绑定表」
（`mem/addr.rs`、`asm/constraint.rs`），但那样每新增一个读侧就要记得查；而且它与
GVN 已有的逸出分析形成两套判据。现在统一到一处：

| 环节 | 改动 |
|---|---|
| `ssa_gvn::walk_expr_addr_of` / `walk_stmt_addr_of` | 去掉 `_ => {}`，**穷举**所有 `Expr`/`Stmt` 变体。旧版把语句表达式 `({...})`、复合字面量/初始化列表、`AsmExpr` 操作数、`va_arg` 全部静默不递归 ⇒ 那里的 `&x` 根本不会被报为逸出（GVN 同一个判据也跟着漏报） |
| `ssa_gvn::addr_escaped_names` | 新增的唯一入口：返回一个块内所有地址被取走的根名。asm 的 `"m"`/`"Q"`/`"g"` 等内存约束操作数不靠 `AddrOf` 语法，而是单独识别为逸出（它就是「把对象地址交出去」） |
| `Codegen::{collect_addr_escaped, register_binding_allowed}` | 程序级扫一次（含嵌套函数体），局部与 file-scope 两条绑定路径共用同一道门；地址逸出的名字不再写入 `reg_bindings` |
| `tests/fixtures/asm/asm_register_binding_escape.c` | 活性夹具（此前 `register asm` 只在 `_archive/dev/` 里，活性套件零覆盖）：三个方向各一条 —— 取址退回内存、asm `"m"` 约束退回内存、不逸出时绑定仍正确工作（防止退化成「把特性关掉」） |
| `ssa_gvn::tests::addr_escaped_names_covers_every_escape_shape` | 6 种逸出形态 + 一条「不取址必须为空」的反向对照 |

**故意没改的那一半**：「规划期 `pre_register_vars_max` 无脑分槽」保留。因为槽现在
就是逸出对象的退回路径，必须在绑定生效时也存在；把它改成「绑定了就不分槽」会直接
删掉这个退路。剩下的只是浪费帧空间（丢收益，不丢正确性）；要省它得先解决「绑定
生效与否要到整程序扫完才知道，而帧大小在函数体发射前就定死了」这个先后顺序。

**度量**：`cargo test --release` 3083 通过 / 0 失败；OPT-DIFF 仍 18（无回归）；可复现
门禁 5040 组合字节一致；`check_fixtures` 601 个夹具全被引用；`make golden-cross` 6 项
全 OK（arm/mips 产物未变，本项只影响 x86_64 的绑定判定）。

### 7.9 第 20 轮：SCCP 的无类型常量表与调度器的声明盲区（OPT-DIFF 18 → 10）

靠第 18 轮加的 `DCC_OPT_SKIP` / 原有 `DCC_SSA_OFF` 对剩下 18 条逐文件二分，
先修掉两个「单 pass 触发」的根因：

1. **SCCP 的常量表是无类型 i128**（4 个 va_arg 夹具 / 8 条）。`Cast{e,..} =>
   operand_int(e,..)` 把 `(long double)50` 登成整数 50，替常量后变参实参从
   `Var(_tcast_1: LongDouble)` 变成 `Operand::Int(50)`：数值一样但 SysV 参数类从
   MEMORY 变成 INTEGER，caller 不再预留 16 字节溢出区 ⇒ callee 里每个
   `va_arg(long double)` 都读错位置（夹具退出码就是首个读错的序号）。现在
   入表与替入两端都要求「该名字的 SSA 类型是整数类」。新单测同时钉住反向
   对照（整数传播仍要工作），否则这条判据只是「关掉 SCCP」。
2. **调度器看不见用户声明**（vec_cast.c / 2 条）。`PreservedDecl` 把整条 `Decl::Var`
   保留成一条指令，它**没有 dest**，所以 `inst_defs`/`inst_def_bases` 都不报它，
   名字→基名表里也没有它供出的 `v.1` ⇒ 基名序链上有洞，P5 能把声明挖到读它的
   值之后（实测 -O3 下 `v2si v = {..}; (unsigned long long)v` 拿到全 0）。同时
   `base_of` 现在也覆盖「只被读过、从没当过 dest」的名字。
   这条长期藏在 1. 后面：被折成常量后根本没有可错序的依赖边。两个都修，而不是
   把折叠改回去。`vector_size_cast_in_expression_run_returns_zero` 改成四档全跑
   ——只跑 -O0 时这类 -O3 缺陷对测试套件完全不可见，只能靠 OPT 门禁兜到。

### 7.10 未修：vla.c（GVN 触发），以两种被实测否定的候选方案为证据

现象：`z->data[i]` 的三次地址计算被合并成一个 `_tmem_16`；它不再「单次使用」，
重建时就不能再内联，于是变成一个具名局部——而**局部的存储语义与它装的值不一回事**：
codegen 对数组类型的局部按「对象住在自己的槽里」给地址（`lea slot`），而槽里存的
其实是那块对象的地址 ⇒ 读写全落到本地帧（`zero_len_test`/`flex_arr_test` 返回 -2）。

三个看上去合理的修法都被实测否定（都已回退，证据留在 `ssa_ident_expr` 的注释里）：

| 候选 | 实测结果 |
|---|---|
| 重建时把数组类型的合成临时值衰减成 `Ptr(elem)`（声明与使用处同一规则） | **修好 vla.c 全部四档**，但 `may_alias.c` 在 -O2/-O3 段错误 ⇒ 同一个「数组类型」在本 IR 里有两种存储语义，类型本身分不出来 |
| GVN 不给数组类型的结果编号（`val_key` 路线） | 数组临时值确实不再被合并（探针确认 `_tmem_7/10/13/16/19/23` 各自 uses=1），**但 vla.c 仍 rc=12** |
| 在生产端衰减（`lower_expr` 的 Member 分支把数组成员的类型直接做成 `Ptr(elem)`） | **更糟**：vla.c 与 may_alias.c 都段错误 ⇒ codegen 把 `MemberLoad{ty: Ptr}` 当成「从该地址再取一个指针」，类型不是标签而是加载宽度 |

may_alias 为什么会翻车（新证据，不是猜测）：把该类临时值声明成 `Ptr` 后，-O2 对
帧内结构体的成员访问会多出一次**整体拷贝**：`may_alias.c` 的 `aligned_alias_test` 在
-O0 是 `lea -80(%r13), %rax`（直接给成员地址），而 decay 版本变成
`lea -80(%r13),%rsi; lea -88(%r13),%rdi; mov $2,%rcx; rep movsq` —— codegen 把
「取子对象地址」当成了「按值产生一个聚合」并复制了整个结构体。也就是说：帧内对象需要
数组类型（地址=自己的槽），堆块里的柔性数组需要指针类型（槽里存的是地址），二者在
IR 里目前是同一个东西。

三条一起指向同一个结论：**「子对象的地址」在本 IR 里没有自己的表示**。它今天被塞进
一个「数组类型的值」里，于是同一个类型背着两种存储语义（别名 vs 住在自己槽里的聚合），
任何按类型猜的修法都会在另一边上翻车。注意第二条：即便 GVN 不再合并，`flex_arr_test`
里单次使用的 `_tmem_N` 也照样被物化（`inline_window_safe` 因为中间有存/放调用而拒绝
内联），所以这不是 GVN 的锅，GVN 只是让它更早显形。

下一步（不是再猜，而是照这个结论做）：给 IR 一个明确的「取子对象地址」形态（新
`InstKind::MemberAddr`/`IndexAddr`，或给现有 `MemberLoad`/`IndexLoad` 加 `is_addr` 位），
让 codegen 按地址交付、重建按指针声明，两端不再靠类型推断。验收对：`vla.c`（堆内
柔性数组）与 `may_alias.c`（帧内结构体的数组成员）必须同时四档与 -O0 一致。

本轮度量：`cargo test --release` 3084 通过 / 0 失败；OPT-DIFF **18 → 10（5 个文件）**；
可复现门禁 5040 组合字节一致；`check_fixtures` 全引用、selftest 5/5。

### 7.11 第 21 轮：赋值表达式的值与「中间那一层宽度」（OPT-DIFF 10 → 8）

`chained_assign_truncation.c` 在 -O2/-O3 走 `abort()`。上一轮把范围收到最小可复现：
`unsigned f(void){ ... ul = us = l2 = s2 = -1; return ul; }` 单独编译**是对的**，
真正错的是全局量那组 —— `short gs; long gl; unsigned short gu; gl = gu = gs = -1;`
在 -O2 得到 `gl = -1`，而 -O0 与 gcc 都是 `65535`。顺着这条线挖出**两个独立缺陷**：

1. **构建器把预派发的 `target` 直接当 `Cast` 的 dest**（`lower_expr` 的 Cast 分支）。
   `long w1 = (unsigned short)-1;` 走 `lower_expr_into(rhs, w1.1)`，而 `w1.1` 的类型是
   `long`，于是那条 Cast 的 dest 类型成了 `long`，`(unsigned short)` 这一层的宽度与符号
   在 IR 里**根本不存在了**。SCCP 之后按 `dest.ty` 掩码就掩成 64 位，常量停在 -1。
   现在只有当 `target.ty == 表达式自己的类型` 时才复用 target，否则先按表达式自己的类型
   算出结果、再显式转成目标类型（两层 Cast 都在 IR 里）。
2. **给具名对象赋值后返回原始 RHS**（`store_to_named_object`），违反 C11 6.5.16p3：
   赋值表达式的值是**存入之后**的对象值。局部量那条路径靠 `lower_expr_into` 把 RHS 直接
   落进左值的新版本，天然带类型，所以一直是对的；全局量这条路径漏了，于是链式赋值把
   中间宽度吃掉。现在在标量且类型不一致时补一条 `Cast`（不是回读内存，因此不给 volatile
   全局增加可观察访问）。

两个缺陷都不是优化器的错，SCCP 只是把丢了类型的 IR 忠实地折叠出来而已。夹具
`chained_assign_truncation.c` 补了一组新断言，把两类都钉住：`w1 = (unsigned short)-1`
（局部，缺陷 1）、`long w2 = (unsigned char)300`、`uw = 70000` 后再 `w3 = (short)uw`，
以及 `jv = (iv = (unsigned short)-1)`（表达式值，缺陷 2）—— 期望值全部按 gcc 实测核对。

度量：OPT-DIFF **10 → 8（4 个文件）**；`cargo test --release` 3084 通过 / 0 失败 / 零
warning；可复现门禁 5040 组合字节一致；`check_fixtures` 全引用；新代码 clippy 零命中。

### 7.12 未修项的最小可复现（第 21 轮定位，尚未动手）

剩下 4 个文件都还开着，但每一个都已经缩到「一句话可复现」，下一轮不需要再从头找：

**`complex_abi.c`（-O2 起 rc=1）**——同一个函数里四种写法只差一条，实测只有
「字段式写入后当值返回」这条坏：

| 函数体 | -O0 | -O2 | gcc |
|---|---|---|---|
| `double _Complex z = r + i*I; return z;` | ✓ | ✓ | ✓ |
| `double _Complex z; z = r + i*I; return z;` | ✓ | ✓ | ✓ |
| `double _Complex z; __real__ z=r; __imag__ z=i; return z;` | ✓ | **坏**（返回 (2, 6.95e-310)，即另一函数的数据） | ✓ |

也就是说：`_Complex` 局部被 `__real__`/`__imag__` 逐字段写完后作为返回值读出时，
SSA 往返之后拿不到那两次字段写的结果；坏的是**读**侧（写侧本身在 -O0 下是对的）。
还没确认的两件事（下一步先量，不要先假设）：字段写有没有为该局部发出 `AddrOf`，
从而 `z` 是否真的进了 `addr_taken`（决定 `is_mem_only_object` 对它是否为 true）；
以及重建后 `return z` 落成什么形态。做法：打印该函数往返前后的 IR 与重建后的 AST。
之后才谈修法：给 `_Complex` 局部一条完整的内存回读，还是在重建里禁止把复杂对象
当标量值取。

**`ld80_arith.c`（-O2 起 rc=1）**：80-bit `long double` 的**对象位型**判据夹具
（ memcmp 期望的是 gcc 的 x87 80-bit 位型）。-O0 与 gcc 逐字节一致，-O2 起差 ⇒
SSA 往返把 long double 的值通道压回了 f64 精度那一类。这属于
docs/longdouble_80bit_migration.md 的未完部分，不是 opt pass 的局部问题。

**`agg_param_reg_stack_split.c`（-O2 起 rc=6）**：聚合参数在「寄存器槽 / 栈槽」拆分上的
判据；-O0 对、-O2 错 ⇒ 与 §7.10/§7.12 同源的那一类：**聚合值在本 IR 里没有被当成对象**，
只是被当成一个 SSA 值传来传去。

三条一起指向同一个结构性缺口（跟 §7.10 的「子对象地址没有表示」是同一件事的不同侧面）：
**值种类（地址 / 聚合 / 80 位浮点 / 复数）在 SSA IR 里都靠 Type 猜，而不是各自有形态**。
按这个方向做的下一步能一次收掉多个夹具；继续按类型打补丁则每个文件都要重来一遍。

### 7.13 第 22 轮：`_Complex` 的两条静默丢失（OPT-DIFF 8 → 6）

`complex_abi.c` 现在四档全绿。它是两条互相独立的缺陷，都靠同一套最小可复现定位
（先缩到一句话，再问「IR 里有没有这条指令」，而不是猜 ABI）：

1. **不被认识的赋值左值 = 只做不写**。`lower_lvalue` 只认识 Ident/Deref/Index/Member
   四种左值，`__real__ z = r` 不在其内；调用点在拿到 `None` 时**什么也不发射**，只把
   RHS 当表达式值返回。于是 `double _Complex z; __real__ z=r; __imag__ z=i; return z;`
   过 SSA 后整个函数只剩 `return z`（探针确认两条 Store 根本不在 IR 里），-O2 返回
   未初始化数据。现在与「不可降级表达式」同一条路：记下原因、整个函数放弃 SSA 化，
   留在本来就能正确处理它的 AST 通道 —— 「只做不写」比不优化危险得多。
2. **复数算术在重建时丢了结果类型**。`infer_binop_type` 有指针/向量/浮点/整数各分支，
   唯独没有复数，于是 `x + y`（两侧 `double _Complex`）落到最后的 Int 回退；codegen
   正是靠操作数与结果类型挑选 `gen_complex_binop`，类型一变复数加法就退化成标量运算
   （实测 -O2 得 (1.0e9, 0)，gcc 与 -O0 都是 (6, 8)）。补上复数分支：结果取复数，内部
   类型按两侧实部的通常算术类型（C11 6.2.5p13 / 6.3.1.8），且**排在比较分支之后**，
   所以 `z1 == z2` 之类仍是 int。

两条都配了单测：`unrecognised_assign_lvalue_bails_instead_of_dropping_the_store`（含
`p->a = 1` 必须仍发 MemberStore 的反向对照）与 `infer_binop_type_keeps_complex_results`
（四种算术 + 复数×实数 + 相等运算仍为 int）。`double_complex_abi_run_returns_zero`
从「只跑默认档」改成 -O0/-O1/-O2/-O3 四档全跑 —— 这类只在优化档暴露的缺陷，靠默认档
测试等于没测。

度量：OPT-DIFF **8 → 6（3 个文件）**；`cargo test --release` 3086 通过 / 0 失败 / 零
warning；可复现门禁 5040 组合一致；新代码 clippy 零命中。剩余：`vla.c`（子对象地址无
表示，§7.10）、`ld80_arith.c`（80 位 long double 过 SSA 往返，§7.12）、
`agg_param_reg_stack_split.c`（聚合参数拆分，§7.12）。

### 7.14 第 23 轮：聚合对象的整体赋值在 IR 里没有表示（OPT-DIFF 6 → 4）

`agg_param_reg_stack_split.c` 现在四档与 gcc 一致。但它当场证明的**不是** SysV 的聚合
参数「寄存器/栈槽拆分」，而是 §7.10/§7.12 那条结构性结论的第三个实例：**聚合值在本 IR
里没有被当成对象**。

**最小可复现**（三条写法只差「拷到哪里」，实测只有第三条坏；`-O2`，gcc 全对）：

| 形态 | 修前 -O2 | gcc |
|---|---|---|
| `rb = p.b;` 直接读成员 | ✓ 222 | ✓ 222 |
| `struct P2 t = p; rb2 = t.b;` 局部拷贝 | ✓ 222 | ✓ 222 |
| `g = p; rb3 = g.b;` **给具名对象整体赋值** | **0** | ✓ 222 |

差别只在降级路径：局部拷贝走 `lower_expr_into`，它**定义一个聚合类型的 SSA 变量**，
重建后仍是一条聚合声明，后端按 `size` 搬 ⇒ 对；而 `g = p;` 走 `store_to_named_object`，
发射的是「一条 `Store`，src 是一个 `Operand`」，重建后落成 `*(&g) = p`，后端按**标量**
搬 8 字节 ⇒ 第二个八字节从来没被写过，`g.b` 读出的是栈/数据段残留。

**为什么这条 bail 而不是给 `Store` 补聚合形态**：真修法需要同时改所有发射点与重建点
（成员赋值、初始化列表、按字节拷贝的循环、`Operand` 侧要能表达「一个聚合值的地址」），
而 `Operand` 现在表达不出来 —— 这与 §7.10 里 vla.c 被三种类型规则修法全部否定的缺口
是同一个：**值种类（子对象地址 / 聚合 / 80 位浮点 / 复数）在 SSA IR 里都靠 `Type` 猜，
而不是各自有形态**。形态级改动是下一项独立工作（验收对：`vla.c` + `may_alias.c`）；
本轮按 §7.13 已验证的那条规则处理：**让可能「只做不写 / 只搬一半」的函数整体退出 SSA**，
留在本来就能正确处理它的 AST 通道，把缺口留在诊断（`DCC_SSA_BAIL=1` 打印
`聚合对象的整体赋值无法在 IR 里表达`）里，而不是留在产物里。

**反向对照**（防止退化成「把结构体全关掉」）：`g.b = v;` 走 MemberStore，是受支持的
形态，必须仍进管线 —— 新单测 `aggregate_assignment_to_named_object_bails` 两个断言各钉
一边。`c_agg_param_reg_stack_split_gcc_cross_check` 从「只跑默认档」扩成
-O0/-O1/-O2/-O3 四档 gcc 对照；顺带记一句：`check_opt_equivalence.sh` 只扫 0/2/3 三档，
这条夹具的 -O1 是第一次被覆盖。

**度量**：OPT-DIFF **6 → 4（2 个文件：`c_ld80_arith.c`、`c_vla.c`，各 -O2/-O3）**；
`cargo test --release -j2 -- --test-threads=2` 3087 通过 / 0 失败 / 零 warning；
COMPILE-FAIL 仍 60、ADDR-NORM 仍 2（本轮零新增）；最小复现修后四档均为
`direct=222 local=222 global=222` 且与 gcc 一致，`DCC_SSA_BAIL=1` 打印
`[SSA] f5 不进管线：聚合对象的整体赋值无法在 IR 里表达`。

### 7.15 第 23 轮（下半）：P12 增量路线第一步——绑定表成为寄存器所有权的真源

下一轮方向按选定意见走「P12 增量路线：先补 `ssa_gvn::walk_addr_of` 的遗漏变体，再接
绑定表，并新增活性夹具」。**第一条已在第 19 轮做完**（`walk_expr_addr_of` /
`walk_stmt_addr_of` 去掉了 `_ => {}` 并穷举所有变体，`addr_escaped_names` 是唯一入口，
见 §7.8），本轮因此从「接绑定表」起步 —— 一上手就撞出两条比 GVN 严重得多的存量错值，
而且它们**都不在优化档**：-O0/-O1 错，-O2/-O3 因为 SSA 往返换了表达式形状恰好躲过。

**撞出来的三条（全部实测，修前 vs gcc）**：

| 形态 | 修前 -O0 | gcc | 病灶 |
|---|---|---|---|
| 被调方把变量绑在 %rbx 且自己不做二元式；调用方把左操作数溅在 %rbx 上跨调用保活 | `r=1500` | `r=500` | 绑定从不登记 `callee_saved_used` ⇒ 序言那条保存被 `patch_callee_saves` 改成 nop，本函数毁掉调用者的寄存器 |
| `register long v asm("rbx") = 7;` 之后同函数来一次 `(q + r) * g(s)` | `x=50 v=25` | `x=32 v=7` | 绑定从不登记 `reg_allocated` ⇒ 表达式溅出池把**同一个 %rbx** 当临时值发了出去，直接盖掉变量的家 |
| file-scope `register long gv asm("r14")` | 整份目标文件里 `mov %r14,-0x20(%r13)` 出现 **0 次**，而函数体确实在往 %r14 写 | — | 第三条路：`compile_func` 开头就填好绑定表，位图随后被 `reset_reg_tracker()` 清零 |

**根因只有一个**：`reg_bindings` 记录「某个名字住在某个物理寄存器」，却没有任何人把那个
寄存器登记为**已占用**。x86_64 后端有两个互不相干的寄存器消费者 —— 表达式溅出池
（`alloc_callee_saved`，按 `reg_allocated` 位图分配）与序言的 callee-saved 保存表（按
`callee_saved_used` 位图回填）—— 两个都不看绑定表，于是同一个寄存器被三个人同时拥有，
且没有任何一处会报错。这与 §7.8 的逸出判据是同一个模式：**所有权判据一旦有两套，弱的
那一套决定产物**。

修法（仍是「单一入口」，不做「读侧到处查表」）：

| 改动 | 作用 |
|---|---|
| `Codegen::claim_bound_register` | 绑到 callee-saved 池里的寄存器时同时置 `reg_allocated` 与 `callee_saved_used` 两个位。文档里划出边界：绑到 caller-saved / 固定用途寄存器（rax/rcx/rdx/rsi/rdi/r8/r9/rsp/rbp/r13）不在这道门的能力之内 —— 表达式求值每时每刻都在用它们，GCC 为此建的是完整干涉图而不是一个位图 |
| `reset_reg_tracker` | 清零位图后按 `reg_bindings` 重新登记，覆盖 file-scope 绑定那条「先填表、后清零」的顺序 |
| `free_callee_saved` | 绑定中的寄存器不允许被标回空闲，防止未来新增的释放点再犯同一个错 |
| `tests/fixtures/asm/asm_register_binding_clobber.c` | 新增活性夹具：两条运行时形态 + `pinned()` 反向对照（绑定必须仍然生效，判据不能退化成「把 register asm 关掉」）+ file-scope 形态 |
| `asm_register_binding_owns_its_register` | 四档逐档 gcc 对照；静态反向对照 `mov %rax, %rbx` 钉住绑定；产物字节断言钉住序言保存（修前该字节序列出现 0 次） |

**代价**：绑定生效期间临时值池少一个寄存器（4 → 3），更深的表达式退回帧槽。这是所有权
模型的必然结果而不是回归 —— GCC 同样是在干涉图里减掉已被占用的寄存器。

**本轮新测到的两条同类缺口（同一族，都已缩到一句话可复现；宽度那条当轮就闭合了，见
§7.16）**：

| 形态 | 实测 | 病灶 |
|---|---|---|
| ~~`register unsigned int v asm("rbx"); v = 0x100000001UL; unsigned long back = v;`~~ **已修（§7.16）** | -O0 `back=4294967297`，gcc 与 -O2 都是 `1` | 绑定路径是两次裸 64 位 `mov`：`mov_to_stack` 不按声明宽度截断，`mov_from_stack` 不做符号/零扩展 —— 而栈槽路径的宽度口径（`movsxd` / `movzx` / 32 位 store）是专门修过的（`access.rs` 里那段注释写着「按变量声明类型宽度存储」）。P12-B 要把 `int`/`char` 变量也灌进绑定表，必须先闭合这条 ⇒ 已闭合 |
| 四个 `"=q"` 输出 + `register long v asm("rbx")` | -O0 `after=22`，gcc `after=100` | `GP_LEGACY_POOL = [rax,rbx,rcx,rdx]` 服务 `q`/`Q` 约束，而 asm 规划器（`AsmRegisterPlanner::new` 只吃 clobber 列表）不知道绑定表 |

第二条**不能**只把绑定寄存器加进规划器的排除集合：`q` 的可用池只有 4 个，一旦
耗尽，现有回退 `pool[seed % 4]` 会返回一个**已经占用**的寄存器，那只是把错值换了
一个方向。要先定的是「`q` 池耗尽时怎么办」—— gcc 在这种情况下报 impossible
constraint，而 dcc 已经有 W0005「impossible constraint」这条诊断可复用。因此它与
P12-B 的占用模型一起设计，本轮不半途改。

**为什么这仍算 P12 的增量落地**：§7.3 已说明 P12 的字面形态（`ast::Ident` 携带寄存器号 +
codegen 不再分配 + 真 SPILL/RELOAD）要重写发射模型，而实质价值是「让分配结果进入产物且
正确」。本轮先把「谁拥有哪个寄存器」这一条判据闭合：自动分配灌进 `reg_bindings` 之前，
这张表必须已经是所有消费者公认的真源，否则自动分配只是把同一个冲突从用户手里接过来。
下一步（P12-B）才是 SSA 层活性区间 → 挑变量 → 灌进这张表，并且按 §7.5 的教训用**产物
差异**证明它真的生效。

**度量**：`cargo test --release -j2 -- --test-threads=2` 3088 通过 / 0 失败 / 零
warning；OPT-DIFF 仍 **4（2 个文件）**（本轮改的是后端发射层，没有新增等价性债）；
可复现门禁 5040 组合字节一致；`make golden-cross` 校验 6 项全 OK（arm/mips 产物未变，
本项只影响 x86_64）；`check_fixtures` / `check_fixtures_selftest` 5/5 全过。

### 7.16 第 23 轮（下半之二）：绑定路径的宽度口径与栈槽路径对齐

§7.15 留下的两条未闭合项里先做了「绑定路径按声明宽度存取」—— 它同时是 P12-B 的前置：
自动分配要能把 `int`/`char` 变量灌进绑定表。

**病灶**：`mov_to_stack` / `mov_from_stack` 的绑定分支各是一次裸 64 位 `mov`，而栈槽分支
的宽度规则是专门修过的（窄写只写那么多字节、读时按符号性 `movsxd`/`movzx`）。同一个变量
因此在两种形态下给出两个值。最小复现（新夹具 `tests/fixtures/c/register_binding_width.c`，
修前 = `75fc854` 的编译器，-O0 实测）：

| 语句 | 修前 -O0 | gcc |
|---|---|---|
| `register unsigned int v asm("rbx"); v = w; bv = v;`（`w = 0x1000000ABUL`） | `bv=0` | 171 |
| `register unsigned int gvu asm("r15"); gvu = w; gv = gvu;`（file-scope） | `gv=4294967467` | 171 |
| 控制：`s = -1`（signed int）、`c = 200`（signed char）、循环里 `c += 1` | ✓ | ✓ |

`bv=0` 里混着 §7.15 的缺陷 2（%rbx 还被临时值池借走了一次），`gv=4294967467` 才是纯宽度
口径问题 —— 而它也只有「宽源赋给窄绑定变量」才会暴露：`c = 200` 这种写法在赋值处就已经
把值转成了目标类型，寄存器里本来就是 -56，所以窄绑定的日常用法看上去一直是对的。

| 改动 | 作用 |
|---|---|
| `Codegen::var_access_shape` | 宽度口径的唯一真源，返回 `(字节数, 4 字节有符号, 窄有符号枚举, 是聚合)`。原来这段判断只是 `mov_from_stack` 里的一个局部闭包，现在提出来给两条通路共用；类型来源是 `var_map`（局部）→ 绑定表携带的声明类型 → 8 字节回退 |
| `Codegen::mov_reg_truncate` | 写入绑定寄存器时按声明宽度截断成**规范形态**（低位有效、高位为 0），于是读出侧无符号只需普通 64 位搬运 |
| `Codegen::mov_reg_extend` | 读出时按符号性补 `movsx`/`movsxd`，与栈槽路径同口径 |
| `reg_bindings: HashMap<String, (u8, Type)>`（连 `global_reg_bindings` 一起） | 寄存器与声明类型**同表携带**。file-scope 的 `register asm` 变量在收集全局时被 `continue` 掉，既不在 `var_map` 也不在 `globals_raw`，宽度只能由绑定表自己带 —— 分两张表就一定会漂移 |
| `tests/fixtures/c/register_binding_width.c` + `asm_register_binding_honors_declared_width` | 四档逐档 gcc 对照；夹具放在 `tests/fixtures/c/` 里，因此 OPT 等价门禁从此也覆盖这条 |

**本轮又实测抓到的一条自身缺陷（写下来，因为它正是这类改动的典型陷阱）**：第一版
`mov_reg_truncate` 的 4 字节分支用了 `89 /r`，而 `89` 的方向是「reg 是源、r/m 是目的」，
`mov()` 用的 `8B` 才相反 —— 结果是把变量的**旧值**当输入搬进去，所有测试值变成栈垃圾。
是靠「先量再判」当场发现的，不是靠读代码。

**度量**：`cargo test --release -j2 -- --test-threads=2` 3089 通过 / 0 失败 / 零
warning；OPT-DIFF 仍 **4（2 个文件）** —— 新夹具本身进了语料，修前它会以
`-O0 vs -O2` 的形式多出一条 OPT-DIFF，现在四档一致；可复现门禁从 5040 涨到
**5058 组合**（新夹具带来的语料）且全部字节一致；`make golden-cross` 6 项全 OK；
`check_fixtures` 全过（新夹具被测试引用）。

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

