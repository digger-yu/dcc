# C++ Native 路径基础能力补齐方案

> **路径勘误（2026-10，SSA 目录化）**：本文里的 `src/ssa*.rs` / `ssa_passes` 等旧路径**不改写**；
> 现 `src/ssa.rs` → `src/ssa/`（已拆为多文件）、`src/ssa_passes.rs` → `src/ssa/passes.rs`。
> 对照表见 [`docs/ssa-module-layout-plan.md` §10](./ssa-module-layout-plan.md)。

- 状态: 提案
- 前置: legacy lower-to-C 路径已淘汰（driver.rs 改造完成），native lower_ir 为唯一后端
- 目标: 补齐 C++ native 路径的基础诊断与语义能力，对齐 C 端已实现的 fallthrough/volatile 水准，逐步消除 legacy 淘汰后的测试失败
- 约束: 不追求一步到位，按优先级分阶段实施；"特性不全可以慢慢补"，但正确性问题优先

## 0. 背景与现状

### 0.1 已完成工作（本次提交）

1. **Fallthrough 最佳方案重构（C 端）**: `Stmt::Fallthrough` 变体 + `last_effective_stmt` 从后向前扫描 + W0501/W0502 双警告 + `-Wimplicit-fallthrough` CLI 开关
2. **Volatile 局部变量修复（C 端）**: 标量 volatile 局部变量绕过 SSA 寄存器化，读写走 AddrOf+Load/Store；非 arrow MemberLoad/MemberStore 的 is_volatile 修复
3. **Legacy 路径淘汰**: driver.rs 移除 C++ 字符串降级→临时文件→子进程二次编译的 legacy 路径，native lower_ir 为唯一后端
4. **C++ lower_ir volatile 验证**: 解引用 Load/Store、MemberLoad/MemberStore 已正确携带 is_volatile

### 0.2 遗留问题总览

| 类别 | 问题 | 影响范围 | 优先级 |
|------|------|----------|--------|
| Fallthrough | C++ 端 `[[fallthrough]]` 属性完全不支持 | C++ switch 诊断缺失 | 高 |
| Fallthrough | C++ 端 switch 隐式贯穿无警告 | C++ 代码质量 | 高 |
| Volatile | C++ 端局部 volatile 变量可能被 SSA 寄存器化 | C++ volatile 语义正确性 | 高 |
| Volatile | C++ 端数组元素访问 volatile 标志需确认 | C++ volatile 数组 | 中 |
| 正确性 | C 端 PostInc/PreDec 非 volatile 路径 Store 到 SymAddr（局部变量错） | C 局部变量 ++/-- | 高 |
| 测试 | 57 个 C++ 测试失败（legacy 淘汰后） | 回归测试 | 中 |
| 特性 | native 仅 20/29 特性支持 | C++ 能力覆盖 | 中 |

## 1. Fallthrough: C++ 端对齐 C 端最佳方案

### 1.1 问题分析

C++ 端现状（`src/cpp/`）:
- `[[...]]` 标准属性语法完全未实现（lexer 不识别 `[[`，parser 无属性处理）
- `CppStmt::Switch` 的 arms 结构中无 fallthrough 相关字段
- `HirStmtKind::Case/Default` 无 fallthrough 支持
- `sema.rs` 无 switch 语义检查
- GNU `__attribute__((fallthrough))` 仅在声明尾缀被跳过，无语句级支持

C 端已实现的最佳方案（可对齐）:
- `Stmt::Fallthrough` 作为一等语句存在于 case body
- `last_effective_stmt(body)` 从末尾向前扫描最后一条有效语句
- 最后是 Fallthrough → 合法；最后语句终止控制流 → 不贯穿；否则 → W0501 警告
- Fallthrough 存在但不在末尾 → W0502 misplaced 警告

### 1.2 实施方案

**阶段 A: `[[...]]` 属性语法基础（前置）**
- lexer: 识别 `[[` / `]]` token（`TokKind::LBracketLBracket` / `RBracketRBracket`）
- parser: 新增 `parse_std_attributes()`，解析 `[[attr1, attr2(args), ...]]`
- 属性存储到 `Attrs` 结构体（目前仅 `decl_attrs` 用于声明，需扩展到语句/表达式）
- 语句级属性: `[[attr]];` 解析为属性语句（空操作 + 属性集合）

**阶段 B: `[[fallthrough]]` 语句解析**
- 在 switch case body 解析中，遇到 `[[fallthrough]];` 时生成 `CppStmt::Fallthrough`（新增变体）
- `__attribute__((fallthrough));` 同样映射为 `CppStmt::Fallthrough`
- HIR 层新增 `HirStmtKind::Fallthrough`
- lower_ir 层映射为 `InstKind::Nop` 或空操作

**阶段 C: fallthrough 诊断（对齐 C 端最佳方案）**
- 在 sema 或 parser 阶段实现 `last_effective_stmt` 逻辑
- 对每个 switch arm（最后一个 arm 除外）检查:
  - 最后有效语句是 Fallthrough → 合法
  - 最后有效语句终止控制流（break/return/continue/goto/throw）→ 跳过
  - 其他 → W0501 隐式贯穿警告
  - 有 Fallthrough 但不在末尾 → 额外 W0502 misplaced 警告
- 警告受 `-Wimplicit-fallthrough` 控制（C 端已有 CLI 开关，C++ 端复用）

### 1.3 验收标准

- `[[fallthrough]];` 在 C++ switch case 中正确消除警告
- `__attribute__((fallthrough));` 同样生效
- 未标注的隐式贯穿报 W0501，定位到 case 标签
- fallthrough 不在 case 末尾报 W0502，定位到 fallthrough 语句
- 与 C 端行为一致

## 2. Volatile: C++ 端语义完整性

### 2.1 问题分析

C++ lower_ir 现状:
- 解引用 Load/Store（`*p`）: 已从 `inner_expr.ty.is_volatile()` 推导 is_volatile ✅
- MemberLoad/MemberStore（`obj.field` / `p->field`）: 已通过 `obj_base_is_volatile()` 推导 ✅
- IndexLoad/IndexStore: **不存在**（数组访问可能走其他路径，需确认）
- 局部变量 SSA 化: **需确认是否存在与 C 端相同的寄存器化问题**

C 端已修复的问题（C++ 端需对齐）:
- 标量 volatile 局部变量被 SSA 常量折叠 → 走 AddrOf+Load/Store
- 非 arrow MemberLoad/MemberStore is_volatile 硬编码 false → 从 local_quals 推导

### 2.2 待确认项（实施前需调研）

1. **C++ lower_ir 中数组访问走哪条路径？** 是直接生成指针运算 + Load/Store，还是有 IndexLoad/IndexStore？
2. **C++ lower_ir 中局部变量是否被 SSA 版本化？** 如果是，volatile 标量同样会被常量折叠
3. **C++ lower_ir 的优化 pass 与 C 端是否共用 ssa_passes.rs？** 如果共用，DCE 的 is_volatile 判断逻辑一致

### 2.3 实施方案

**阶段 A: 调研确认（1-2 天）**
- 追踪 C++ lower_ir 中数组访问、局部变量读写的完整路径
- 确认是否存在与 C 端相同的 volatile 语义漏洞
- 输出: 漏洞清单 + 严重程度评估

**阶段 B: 局部 volatile 标量修复（如需要）**
- 对齐 C 端方案: 标量 volatile 局部变量的读取走 AddrOf+Load，写入走 AddrOf+Store
- 复合类型（数组/结构体）保持原有路径，因子访问已带 is_volatile
- 修复位置: `lower_ir.rs` 的变量读取/写入逻辑

**阶段 C: 数组访问 volatile 修复（如需要）**
- 若数组访问走指针运算 + Load/Store: 确保 Load/Store 的 is_volatile 正确
- 若走 IndexLoad/IndexStore: 补 is_volatile 推导（从数组元素类型的 quals）

**阶段 D: 验证测试**
- 新增 C++ volatile 测试用例（对标 C 端 `ssa_volatile_semantics.c`）
- 覆盖: 局部标量、全局变量、指针、数组、结构体成员、箭头访问

### 2.4 验收标准

- C++ 中 `volatile int x = 0; return x + x;` 在 -O2 下生成两次内存读取
- volatile 数组元素访问在 -O2 下不被 DCE 消除
- volatile 结构体成员访问正确携带 is_volatile
- 与 C 端行为一致

## 3. C 端 PostInc/PreDec 修复

### 3.1 问题

`src/ssa.rs` 中 `Expr::PostInc/PreInc/PostDec/PreDec` 处理:
- 非 volatile 路径下，Store 到 `Operand::SymAddr(name, ty)`
- `SymAddr` 是全局符号地址，对局部变量语义错误
- 表现: 非 volatile 局部变量的 `x++` 会段错误（O2 下）

注意: 此问题在本次修改前就存在，属历史 bug。但因本次修改触及了同一代码区域，应一并修复。

### 3.2 根因

PostInc/PreDec 的原始实现可能只考虑了全局变量场景，未正确处理局部变量。局部变量的自增应该通过 SSA 版本化完成（`x_n+1 = x_n + 1`），不需要 Store 到内存。

### 3.3 实施方案

- 非 volatile 路径: 移除 Store 指令，直接用 SSA 版本化（define 新版本，值为 new_dest）
- volatile 路径: 保持 AddrOf + Load + 计算 + Store（本次已修复）
- `nv` 变量需要正确关联到 new_dest 的值（通过 copy 指令或直接使用 new_dest）

### 3.4 验收标准

- `int x = 0; x++; return x;` 在 O0 和 O2 下均返回 1
- 现有测试全部通过（无新增失败）

## 4. Legacy 淘汰后测试失败处理

### 4.1 现状

57 个 C++ 测试失败，全部为 legacy 路径支持但 native 路径不支持的特性:
- m17_* (17 项): auto_deduce, explicit_ctor, decltype_auto, final_override, friend, if_constexpr, inheriting_ctor, mutable, new_array, noexcept, placement_new, weak_vtable_multi_file_link
- m18_* (15 项): concept, chrono, condition_variable, filesystem, flat_map, modules, expected, multi_index, regex, sfinae_smoke, ranges, thread_mutex
- m26_* / m27_* (15 项): auto_deduction_runtime, coroutine_runtime, coroutine_state_machine, enum_semantics_runtime, gcc_compat_runtime, member_parser_runtime, module_*, odr_*, noexcept_expr_runtime, virtual_inheritance_runtime, member_ptr_deref_typed, modules_odr, mutable_const_method, template_specialization, typeid_dynamic_rtti
- p2_* (6 项): member_ptr_dotstar, typeid_rtti, template_arg_syntax, new_arr_mangler, new_array_alloc, placement_new
- 其他 (4 项): coroutine_co_await_runtime, cpp11_using_type_alias_compile, cpp20_features_compile, cpp98_iterator_traits_compile, cpp98_iostream_clog_ws_compile, cpp_template_vector_int_instantiation, cpp23_features_compile, cpp_vector_header_is_template_class, semantic_enum_type_traits_runtime

### 4.2 策略

不追求一次性修复全部 57 个失败。按以下优先级逐步补齐:

**P0 — 阻塞性问题（影响基础功能）**: 暂无（基础 C++ 类/函数/继承/模板单态化已工作）

**P1 — 高频使用特性**（native 能力提升）:
- 模板相关: 全特化、偏特化排序、模板模板参数、可变参数模板
- 语义核心: 重载决议、ADL、SFINAE
- 这些是 `dcc-compile-gxx-source-plan.md` 中 Phase 1-2 的内容

**P2 — 诊断与正确性**:
- Fallthrough 诊断（见第 1 节）
- Volatile 语义（见第 2 节）
- static_assert 语义

**P3 — 运行时/库依赖**:
- 异常（SJLJ → DWARF）、RTTI 完整化
- 线程、文件系统、正则等库特性（依赖 libstdc++）

### 4.3 与 dcc-compile-gxx-source-plan.md 的关系

本方案是该大方案的**前置基础层**:
- 本方案聚焦 native 路径的**正确性与基础诊断**（fallthrough/volatile/测试修复）
- gxx-source-plan 聚焦**特性缺口**（模板/重载/constexpr 等）
- 两者并行推进，互不阻塞

## 5. 实施路线图

| 阶段 | 内容 | 预计工作量 | 依赖 |
|------|------|-----------|------|
| P0 | C 端 PostInc/PreDec 非 volatile 路径修复 | 0.5 天 | 无 |
| P1 | C++ fallthrough 阶段 A: `[[...]]` 属性语法基础 | 2-3 天 | 无 |
| P2 | C++ fallthrough 阶段 B+C: fallthrough 解析与诊断 | 1-2 天 | P1 |
| P3 | C++ volatile 阶段 A: 调研确认 | 1 天 | 无 |
| P4 | C++ volatile 阶段 B+C: 修复（如需要） | 1-2 天 | P3 |
| P5 | C++ volatile 阶段 D: 测试验证 | 0.5 天 | P4 |
| P6 | 逐步补齐 native 特性，消减 57 个测试失败 | 长期 | 见 gxx-source-plan |

## 6. 验收标准（DoD）

1. C++ 端 fallthrough 诊断与 C 端行为一致（W0501/W0502 + CLI 开关）
2. C++ 端 volatile 语义正确（局部变量不被寄存器化、数组/成员访问带 is_volatile）
3. C 端 PostInc/PreDec 历史 bug 修复
4. 新增测试覆盖上述功能，且无回归
5. C++ 测试失败数从 57 逐步下降（不设硬性指标，按优先级推进）

## 附录 A. 相关文件索引

- C 端 fallthrough 参考实现: `src/parser.rs`（`last_effective_stmt` / `check_implicit_fallthrough` / `Stmt::Fallthrough` 解析）
- C 端 volatile 参考实现: `src/ssa.rs`（Ident 读取/Assign 写入/PostInc 的 volatile 路径）
- C++ parser: `src/cpp/parser.rs`（`parse_switch_stmt` / `CppStmt::Switch`）
- C++ HIR: `src/cpp/hir.rs`（`HirStmtKind::Switch/Case/Default`）
- C++ sema: `src/cpp/sema.rs`（目前无 switch/fallthrough 代码）
- C++ lower_ir: `src/cpp/lower_ir.rs`（`lower_dereference` / `lower_field_access` 已有 is_volatile）
- 特性矩阵: `src/cpp/feature_matrix.rs`
- 大方案: `docs/dcc-compile-gxx-source-plan.md`
