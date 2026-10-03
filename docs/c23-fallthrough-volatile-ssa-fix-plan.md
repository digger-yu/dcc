# DCC C23 [[fallthrough]] 诊断 和 SSA volatile 感知问题修复方案

> **路径勘误（2026-10，SSA 目录化）**：本文里的 `ssa.rs:NNNN` / `ssa_passes.rs:NNNN` 旧路径与行号
> **不改写**（当时的事实）。目录化后行号已全部失效，按名字反查：`collect_local_decls` 现在
> `src/ssa/lower.rs`，`inst_is_side_effecting` 现在 `src/ssa/passes.rs`；`bb.local_quals` 等数据形态
> 在 `src/ssa/ir.rs`。对照表见 [`docs/ssa-module-layout-plan.md` §10](./ssa-module-layout-plan.md)。

## 问题总览

| 问题 | 当前状态 | 严重度 |
|------|----------|--------|
| C23 `[[fallthrough]]` 编译时诊断 | 仅语法识别，缺少诊断 | 中 |
| SSA IR 层局部 volatile 变量感知 | AST 层保留限定符，但 SSA IR 层不感知，优化可能错误消除 | 中 |

---

## 问题 1：C23 `[[fallthrough]]` 诊断

### 当前问题
- 现状：parser 已能识别 `[[fallthrough]]` 和 `__attribute__((fallthrough))` 语法，但没有实现编译时诊断：
  1. 不检查 `[[fallthrough]]` 是否出现在 `switch` case 块内正确位置。
  2. 不检测未标记的 case 贯穿（即一个 case 块没有 `break`/`return` 也没有 `[[fallthrough]]`，需要给出警告）。
- 影响范围：用户写出无意贯穿时无法提前发现错误。

### 修复方案

#### 设计原则
- 遵循 GCC/Clang 行为：默认警告未标记的 case 贯穿，遇到正确的 `[[fallthrough]]` 静默。
- 在 AST 层面诊断，不依赖 SSA，尽早反馈给用户。

#### 实施步骤

1. **AST 层面遍历 switch case**
   - 位置：[parser.rs:switch case 解析](file:///root/dcc/src/parser.rs) 后增加诊断检查。
   - 对每个 case：
     - 遍历 case body 的最后一个语句，判断是否会“贯穿”：
       - 如果最后语句是 `break`/`return`/`continue`/`goto label` → 不贯穿，无警告。
       - 如果最后语句就是 `[[fallthrough]]` / `__attribute__((fallthrough))` → 有意贯穿，无警告。
       - 其他情况 → 未标记贯穿，警告。

2. **诊断规则**
   - 如果 `[[fallthrough]]` 不在 `switch` case 块内最后一条语句 → 警告“fallthrough 属性只能位于 switch case 末尾”。
   - case 块为空 → 允许，不需要警告。
   - default case → 不需要警告。
   - 遵循 C23 标准：`[[fallthrough]]` 必须是一个独立的属性声明语句。

3. **诊断级别**
   - 使用 `-Wimplicit-fallthrough` 控制，默认开启警告。
   - 如果用户用 `-Werror=implicit-fallthrough` 升级为错误。

#### 测试要点
- 测试用例覆盖：
  1. 无意贯穿 → 警告。
  2. `[[fallthrough]]` 正确标记 → 无警告。
  3. `[[fallthrough]]` 在错误位置 → 警告。
  4. case 末尾 break/return → 无警告。
  5. 空 case → 无警告。

---

## 问题 2：SSA IR 层局部 volatile 限定符感知

### 当前问题
- 现状：
  1. `collect_local_decls` （旧位 `src/ssa.rs:1516-1565`，现为 `src/ssa/lower.rs`）在 AST→SSA 时收集了局部变量的 `qualifiers` (`func.local_quals`)，重建时恢复。
  2. 但是 SSA IR 层的 `Load`/`Store`/`IndexLoad`/`IndexStore`/`MemberLoad`/`MemberStore` 指令没有 volatile 标记，优化 pass 无法知道访问是否是 volatile 的。
  3. 当前优化还没有 DSE (dead store elimination) 或 load forwarding，所以暂时安全，但是“正确但脆弱”，未来启用这些优化后会错误消除 volatile 访问。

- 根因：volatile 语义要求：
  1. 每个 volatile 访问必须按程序顺序执行，不能被优化器删除、重排序、合并。
  2. 如果局部变量是 volatile，所有对它的读写都必须保留，即使看起来“无用”。
  3. 当前 SSA 不标记，优化会认为和普通变量一样处理。

### 修复方案

#### 设计原则
- 在 IR 层面为所有内存访问指令增加 `is_volatile` 标记。
- 让优化 pass 检查该标记，对 volatile 访问不执行消除/转发。
- 保持重建阶段正确恢复局部变量的 volatile 限定符。

#### 实施步骤

1. **修改 SSA InstKind 内存访问指令**
   - 在 `InstKind::Load`, `Store`, `IndexLoad`, `IndexStore`, `MemberLoad`, `MemberStore` 增加 `is_volatile: bool` 字段。
   - 默认 `false`，不破坏现有代码。

   例如：
   ```rust
   Load {
       e: Operand,
       ty: Type,
       is_volatile: bool,
   },
   Store {
       e: Operand,
       src: Operand,
       is_volatile: bool,
   },
   // ... 同理其他内存访问指令
   ```

2. **在生成 Load/Store 时设置正确标记**
   - 当生成局部变量的 load/store 时：
     1. 查询 `builder.func.local_quals` 获取变量限定符。
     2. 如果 `quals.is_volatile()` → 设置 `is_volatile = true`。
   - 对于全局变量访问：已经在前端标记了 volatile，也要传递标记到 load/store。
   - 对于指针访问：`*ptr`，如果 `ptr` 的 pointee 类型是 volatile → `is_volatile = true`。

3. **修改优化 pass 的检查逻辑**
   - 在死指令消除 (DCE)：如果指令是 volatile load/store，不删除。
   - 在死存储消除 (DSE)：如果存储到 volatile 地址，不消除。
   - 在 load 转发/常量传播：不转发 volatile load，不合并多次访问。
   - 在 alias 分析：保持 volatile 访问的副作用，不消除。

   参考 `inst_is_side_effecting` （旧位 `src/ssa_passes.rs:762-778`，现为 `src/ssa/passes.rs`）：
   ```rust
   InstKind::Load { is_volatile, .. } => is_volatile,
   InstKind::IndexLoad { is_volatile, .. } => is_volatile,
   InstKind::MemberLoad { is_volatile, .. } => is_volatile,
   ```
   这样就不会被 DCE 当成无用指令删除。

   对于 DSE，遍历存储指令时，如果 `is_volatile` → 不尝试消除该存储，也不会消除它前面的存储。

4. **重建阶段验证**
   - `rebuild_block` 已经从 `func.local_quals` 恢复 volatile 限定符到 Decl::Var，不需要改。
   - 验证重建后的 AST 中局部变量仍然保留 volatile，后端可以正确处理。

5. **InlineAsm 已经有 volatile 标记，保持现状。**

#### 风险缓解
- 增加默认参数保持向后兼容，所有现有构造代码不需要全量修改，只在新建指令时传递正确值。
- 分阶段修改：先加字段，再设置正确标记，再更新优化 pass，每步可独立提交测试。

---

## 整体实施计划

| 阶段 | 任务 | 可独立提交 |
|------|------|------------|
| 1 | C23 [[fallthrough]] 诊断实现 | 是 |
| 2 | SSA IR 为所有内存访问指令增加 is_volatile 标记 | 是 |
| 3 | 在生成 Load/Store 时根据类型设置正确 is_volatile | 是 |
| 4 | 更新优化 pass (DCE/DSE/GVN 等) 感知 volatile | 是 |

## 验收标准

1. **[[fallthrough]]**：
   - 未标记的无意贯穿给出正确警告。
   - 正确标记的有意贯穿不警告。
   - 错误位置的 fallthrough 属性给出警告。
   - 不破坏已有语法兼容。

2. **volatile**：
   - 局部/全局/指针 volatile 访问在 SSA IR 都正确标记 is_volatile。
   - DCE/DSE 不删除 volatile 访问。
   - 现有测试套件全部通过，无回归。
   - 重建后的 AST 仍然保留局部变量 volatile 限定符。

---

## 测试策略

- 新增测试目录 `tests/attr/fallthrough.c` 覆盖各种情况。
- 新增测试 `tests/volatile/ssa-volatile.c` 测试 volatile 局部变量读写不被消除。
- 运行现有测试套件验证无回归。
