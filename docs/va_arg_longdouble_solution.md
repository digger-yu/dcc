# va_arg 与 long double 问题彻底解决方案

> 版本：**v2**（2026-09-24 按当前代码现状重写；v1 的代码定位与实施序已过时）
> 状态：方案稿 — 尚未按 v2 开工。
> v1 摸底基线：9.5，单体 `src/codegen/x86_64.rs`。
> v2 摸底：2026-09-24，`src/codegen/x86_64/` 目录化后端 + feature-gated 多架构。

## 问题背景

v1 列出的 5 个问题仍然是本方案的目标：

1. 混合整数/浮点结构体的 va_arg 读取不完整
2. va_arg 聚合参数的入口路径不一致
3. va_copy 真正的状态复制（v1 判断已修复，仅需验证）
4. va_list 传参的数组退化语义脆弱
5. long double 不满足完整 SysV ABI 语义

**但代码基线已经发生根本变化**，v1 中所有 `x86_64.rs#Lxxxxx` 的行号定位全部失效，且其中部分工作已经完成、部分实现在重构中丢失。下文先给现状审计，再给修订后的实施步骤。

---

## 1. 现状审计（2026-09-24）

### 1.1 后端架构变化

- 单体 `src/codegen/x86_64.rs` 已不存在，拆分为 [x86_64/](file:///root/dcc/src/codegen/x86_64) 目录模块（`abi/`、`emit/`、`mem/`、`asm/`、`special/` 等）。
- 该目录化迁移**尚未完成**：[stubs.rs](file:///root/dcc/src/codegen/x86_64/stubs.rs) 仍有约 100 个 `unimplemented!()` 占位，文件头自述"迁移完成后本文件将被删除"。
- 其他后端改为 cargo feature gate：`arm` / `mips`，默认 `default = []` 即**只构建 x86_64**（见 [Cargo.toml](file:///root/dcc/Cargo.toml#L16-L20)）。

### 1.2 实测：x86_64 后端当前不可用

`compile_module` 本身就是桩（[stubs.rs#L19-L21](file:///root/dcc/src/codegen/x86_64/stubs.rs#L19-L21)），因此：

- 编译任意 C 文件（含 `int main(){return 42;}`）都在 codegen 阶段 panic。
- `cargo test --test integration`：**193 passed / 624 failed**；profile 16 failed；perf 4 failed。
- va_arg 相关 19 个测试全部失败。

结论：**va/long double 的剩余工作无法在"后端可用"的前提下增量进行，必须先恢复 x86_64 后端的基本 codegen 能力。** 这是 v2 与 v1 最大的前提差异。

### 1.3 五个问题的逐项状态

| # | 问题 | 状态 | 证据 |
|---|------|------|------|
| 1/2 | 聚合 va_arg 混合类读取、入口对齐 | **丢失，需重做** | `gen_va_arg_struct` / `gen_va_arg_ptr_struct` / `gen_va_arg` / `gen_va_arg_ptr` 全是 `unimplemented!()` 桩（[stubs.rs#L450-L463](file:///root/dcc/src/codegen/x86_64/stubs.rs#L450-L463)）；`resolve_va_list_addr` 桩返回假值（[stubs.rs#L446-L448](file:///root/dcc/src/codegen/x86_64/stubs.rs#L446-L448)）。调用点在 [va_arg.rs](file:///root/dcc/src/codegen/x86_64/emit/expr/va_arg.rs) |
| 3 | va_copy 状态复制 | **丢失，需重做** | 新 x86_64 模块中没有任何 `__builtin_va_start/__builtin_va_copy/__builtin_va_end` 的特殊处理（全模块 grep 无命中），会按普通外部函数调用处理。仅 feature-gated 的 aarch64/riscv64 有 va_copy 实现（[aarch64 call.rs#L30](file:///root/dcc/src/codegen/aarch64/emit/expr/call.rs#L30)、[riscv64 call.rs#L28](file:///root/dcc/src/codegen/riscv64/emit/expr/call.rs#L28)） |
| 4 | va_list 数组退化 | **已完成，无需前端改动** | parser 将 `__builtin_va_list` 直接构造为 4 字段（gp_offset/fp_offset/overflow_arg_area/reg_save_area）结构体的**单元素数组**（[parser.rs#L6075-L6129](file:///root/dcc/src/parser.rs#L6075-L6129)），传参经通用数组→指针退化规则处理；caller 侧也有统一的 array decay（[call.rs#L45-L53](file:///root/dcc/src/codegen/x86_64/abi/call.rs#L45-L53)） |
| 5a | LongDouble 大小/对齐 | **已完成** | `size() = 16`、`align() = 16`（[ast.rs#L1164](file:///root/dcc/src/ast.rs#L1164)、[ast.rs#L1235](file:///root/dcc/src/ast.rs#L1235)），结构体布局与 eightbyte 计数自动正确 |
| 5b | x87 指令发射 | **已迁移存在** | [x87_floating_point.rs](file:///root/dcc/src/codegen/x86_64/special/x87_floating_point.rs) 已含 `fld tbyte`（DB /5）、`fstp tbyte`（DB /7）、f64→80-bit 转换、x87 比较与运算全套 |
| 5c | 80-bit 常量表示 | **未做** | 常量仍统一存 f64（[const_eval.rs#L30](file:///root/dcc/src/const_eval.rs#L30)）。当前路径下 `1.0L` 经 f64 再由 `f64_to_x87_80` 转回，能保证"f64 可表示值"不丢精度，但无法表达真正超出 f64 的 80-bit 字面量 |
| 5d | va_arg long double 读取 | **丢失（随 1/2 一起）** | 读取函数即 5a 表中的桩；x87 发射件已具备，补桩时直接调用 `fld tbyte` + 推进 16 字节即可 |

### 1.4 测试 fixture 现状

v1 计划新增的 5 个 fixture **已全部存在**（命名有差异），且都已在 [integration.rs](file:///root/dcc/tests/integration.rs#L10764-L10837) 注册：

| v1 计划名 | 实际文件 |
|----------|---------|
| `struct_mixed_ints_float.c` | [struct_mixed.c](file:///root/dcc/tests/fixtures/c/va_arg/struct_mixed.c) |
| `struct_mixed_ld.c` + `long_double_constant.c` | [long_double.c](file:///root/dcc/tests/fixtures/c/va_arg/long_double.c) |
| `va_list_param_passing.c` | [va_list_param.c](file:///root/dcc/tests/fixtures/c/va_arg/va_list_param.c) |
| `nested_struct_va_copy.c` | [va_copy_advanced.c](file:///root/dcc/tests/fixtures/c/va_arg/va_copy_advanced.c) |
| （补充） | [aggregate_va_arg.c](file:///root/dcc/tests/fixtures/c/va_arg/aggregate_va_arg.c)、[vla_struct.c](file:///root/dcc/tests/fixtures/c/va_arg/vla_struct.c)、[va_copy.c](file:///root/dcc/tests/fixtures/c/va_arg/va_copy.c) |

因此 v2 **不需要新增 fixture**，目标是让已有测试转绿；仅当 80-bit 字面量精度（5c）开工时，再补充对应用例。

---

## 2. v1 设计中仍然有效的部分

以下技术结论与 v1 一致，重做时直接遵循：

### 2.1 聚合 va_arg（问题 1/2）

- 寄存器保存区路径遍历**所有** eightbyte 分类：`Integer` 从 gp 区读 8 字节；`Sse` 从 fp 区按 16 字节槽读；`SseUp` 复用同一 xmm 高 8 字节；`X87/X87Up/ComplexX87/Memory` 不可能出现在寄存器路径（存在即整体 overflow），断言即可。
- 寄存器保存区路径与 overflow 路径一样，按聚合 `align`（含 >8 的情形）做入口对齐，对齐后再做溢出判断；溢出则整体走 overflow。
- 与 caller 侧 [call.rs](file:///root/dcc/src/codegen/x86_64/abi/call.rs#L64-L84) 的 force_mem 对齐规则保持一致（`align.max(8)`）。

### 2.2 va_start / va_copy / va_end（问题 3）

- `va_start(ap, last)`：ap 为 va_list 数组，取 `&ap[0]` 初始化四字段（gp_offset=0、fp_offset=48*8、reg_save_area 指向序言保存区、overflow_arg_area 指向栈上首个溢出参数）。
- `va_copy(dest, src)`：区分指针（已退化形参）与数组（局部声明）寻址，复制整个 24 字节 va_list，不共享内部状态。
- `va_end`：无操作（nop）。

### 2.3 long double（问题 5）

- 参数/返回走 x87（st0），内存中 16 字节槽、低 10 字节为 80-bit 值。
- va_arg：`fldt (%r9)` 加载，`r9 += 16`。
- 5c 的完整 80-bit 常量表示（独立 FloatBits/F80）维持 v1 的"可选增量"定位：当前 f64 + `f64_to_x87_80` 已覆盖现有 fixture，不阻塞主目标。

---

## 3. 修订后的实施步骤（v2）

> 与 v1 的差异：新增"阶段 0 恢复后端"，删除"新增 fixture"，多后端同步改为 feature gate 下的独立工作。

### 阶段 0：恢复 x86_64 后端基本可用（前置，独立于 va）

1. 补全 `compile_module`：IR → 函数遍历、序言/结语、gen_stmt 主循环。
2. 补全 `gen_expr` 总分发及 [stubs.rs](file:///root/dcc/src/codegen/x86_64/stubs.rs) 中使普通 C 程序可编译所必需的桩。
3. 验收：`int main(){return 42;}` 可编译运行；integration 失败数从 624 显著下降（目标为仅剩与 va/特定未迁移特性相关的用例）。

注：此阶段是目录化迁移的收尾，范围以"消除 panic、恢复主路径"为限，不借机重构。

### 阶段 1：恢复 va_arg 标量与聚合 codegen（问题 1/2/5d）

1. 实现 `resolve_va_list_addr`（Named / InReg / Lvalue 三态，对应 [helpers.rs](file:///root/dcc/src/codegen/x86_64/helpers.rs#L80-L81) 既有枚举）。
2. 实现 `gen_va_arg` / `gen_va_arg_ptr`：标量三路径（gp 区 / fp 区 / overflow），含单 chunk 溢出。
3. 实现 `gen_va_arg_struct` / `gen_va_arg_ptr_struct`：按 §2.1 遍历全分类 + 双路径对齐；long double 分类走 `fld tbyte`、推进 16 字节。
4. 验收：`struct_mixed`、`aggregate_va_arg`、`vla_struct`、`long_double`、`double_precision` 等用例转绿。

### 阶段 2：恢复 va_start / va_copy / va_end（问题 3）

1. 在 call/builtin 分发中拦截三个 `__builtin_va_*`，按 §2.2 实现，不再当外部调用。
2. 变参函数序言：按 ABI 保存 GP/XMM 到 reg_save_area，并正确初始化 overflow_arg_area。
3. 验收：`va_copy`、`va_copy_advanced`、`va_list_param`、`va_list_as_function_param` 等全部转绿。

### 阶段 3：全量回归与（可选）80-bit 常量

1. 运行 `cargo test`、外部 gcc-dg/torture 套件与内核冒烟，确认无回归后删除已落地桩。
2. 可选：按 v1 原案引入 F80 常量表示与字面量精度测试，此时再新增 fixture。

### 多后端同步（非默认构建，独立排期）

aarch64/riscv64 已有 va_start/va_copy 与 `gen_va_arg_struct` 实现（feature gate 后），需在 `--features arm,mips` 下核对：
- 与 x86_64 统一 va_list 寻址/退化语义；
- long double 按各平台 ABI（aarch64/riscv64 为 16 字节）核对；
- arm/mips 的 va_arg 目前仍是空实现（[arm functions.rs#L333](file:///root/dcc/src/codegen/arm/codegen/functions.rs#L333)、[mips baseline_mips.rs#L764](file:///root/dcc/src/codegen/mips/baseline_mips.rs#L764)），不在本方案主线内。

---

## 4. 风险与兼容性

- 最大风险来自阶段 0 的迁移收尾，而非 va 逻辑本身：应以"恢复既有行为"为目标，逐桩对照目录化前语义，避免在补桩时引入新规则。
- 不改变用户 API；long double 布局在 5a 已为 16/16，阶段 1/2 不再改动布局。
- 每个阶段独立可测、可单独提交；阶段 1/2 必须在阶段 0 之后串行。
- 多后端差异被 feature gate 隔离，不阻塞 x86_64 主线验收。

---

## 5. v1 → v2 变更摘要

1. 基线从单体文件改为目录化未完成迁移：新增阶段 0（恢复后端）为硬前置。
2. 问题 1/2/3 的 x86_64 实现状态从"待修补/已修复"改为"**重构中丢失，需重做**"。
3. 问题 4（数组退化）与 5a（大小对齐）、5b（x87 发射件）标记为**已完成**，移出实施清单。
4. v1 计划的 5 个新 fixture 已全部存在，**不再新增**；仅可选 80-bit 常量阶段补例。
5. "同步所有后端"改为 feature gate（`arm`/`mips`）下的独立排期，默认只验收 x86_64。
