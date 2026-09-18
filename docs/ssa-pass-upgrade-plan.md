# P2-1 SSA Pass 基础设施完整升级方案

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
