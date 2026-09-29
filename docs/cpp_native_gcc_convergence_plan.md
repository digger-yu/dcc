# C++ Native 后端对齐 GCC 源码树实施方案

- 状态: 提案
- 前置: `docs/gcc_build_guide.md`（GCC 子集构建门禁）、`docs/cpp_native_backend_implementation_plan.md`（Stage 0–10）、`docs/dcc-compile-gxx-source-plan.md`（libstdc++/gcc/cp 特性缺口盘点）
- 目标: 以 GCC 15.3.0 源码树作为**缺陷发现仪器**，把 C++ native 后端推进到"自身产物可链接、可与 g++ 产物互操作"，并给出到达"用 dcc 编译 gcc-15.3.0"所需的完整缺口清单
- 约束:
  - 零回归：每一步必须保持既有全量测试通过
  - 不追求"编过"，追求"与 g++ 真值逐行一致"（交叉验证是唯一验收口径）
  - 不留分阶段缺口：任何"已知限制 + 注释记录"都必须在本方案里对应到一个 Step
  - 资源约束：所有构建/测试/探测一律限流（`-j2`、`nice`、`JOBS=2`），不允许跑满 CPU

## 0. 现状（实测基线）

### 0.1 探测仪器与基线

`scripts/gcc_tree_cpp_probe.sh` 按组件批量编译 GCC 树中的 C++ TU，并聚合错误频次。当前 `--component=libcpp` 的结果：

| 阶段 | libcpp 16 个 TU 的统一首条阻塞 | 通过数 |
|------|-------------------------------|--------|
| 起点 | `unknown class type: __builtin_va_list` | 0 / 16 |
| 第二站 | `unknown class type: signed short int` | 0 / 16 |
| 第三站 | `unknown class type: _G_fpos_t` | 0 / 16 |
| 第四站 | `undeclared identifier: __builtin_bswap16` (14) / `unknown class type: __locale_data` (2) | 0 / 16 |
| 当前 | `unknown class type: __locale_data` (16) | 0 / 16 |

> 说明一：`0 / 16` 指 native 后端仍未全程走通；此前文档记录的"libcpp 14/14 完成"是 **legacy 路径把降级出的 C 文本交给宿主 `cc` 编译**的结果，不是 dcc 自己的产物（见 §0.3 债务 D1）。
>
> 说明二（方法论硬约束）：上表每一行都必须是**重新构建 `dcc` 之后**的 probe 结果。本表初次编写时曾用 20:41 的旧二进制去验 20:57 已改的源码，得出过"bswap 仍阻塞"的错误结论；`gcc_tree_cpp_probe.sh` 不会自动重编，所以**先 build 再 probe**（已写进 Step 0 与 §4）。

测试基线：`cargo test -j2 --release` 全绿 —— unit 1860、integration 967、`cpp_abi_gcc_cross_check` 9 个分组测试（共 36 条夹具，其中 `tests/fixtures/cpp_abi_c_style_decls/` 为本轮新增的 7 条），其余 errors / perf / profile / reproducibility 等分组均通过。

### 0.2 本轮已彻底修复的缺陷（每项均有 g++ 交叉验证夹具）

| # | 缺陷 | 根因所在 | 修法 | 验证 |
|---|------|----------|------|------|
| 1 | `__builtin_va_list` 未知类名，含 `<stdarg.h>` 的 TU 全灭 | native 管线无 GCC 语言级内建类型知识 | `src/cpp/builtin_decls_prelude.rs` 以普通 C++ 声明注入 native 管线，复用类登记/ABI/lower 全链路 | `long_double_abi`、libcpp probe |
| 2 | `signed short int` 被当成类名 | 多词基本类型按**拼写字符串枚举**，漏组合即降级 | `canonical_simple_type()` 按 [dcl.type.simple] 的「符号性/长度/基准」多重集合归一，词序无关 | `multiword_builtin_types.cpp` + 4 条单测 |
| 3 | `typedef struct {...} N;` 字段全丢 | 成员体被整段跳过，只留 `Typedef{target:Int}` 占位 | `parse_typedef_aggr_decl()` 按定义建真类；匿名聚合体名字由 typedef 赋予 | `typedef_anonymous_aggregates.cpp` |
| 4 | `typedef enum {...} K;` 枚举量不存在 | 同上退化路径 → `undeclared identifier: E_A` | 同上 + 匿名枚举改名 | `typedef_enum_constants.cpp` |
| 5 | `extern "C" int f(...);` 单声明形态降级 Opaque | 只识别块形态 | `at_linkage_spec()` / `parse_linkage_spec()` 统一两形态 | `extern_c_single_declaration.cpp` |
| 6 | `union` 按 `struct` 顺序布局，`sizeof` 偏大、成员互相覆盖 | `is_union` 在 native 链路完全丢失，且 lower_ir 用「字段偏移全为 0」反推 union | `ClassDef/ClassAbi.is_union` 从声明贯通 sema→abi_layout→lower_ir，废除反推判据 | `typedef_anonymous_aggregates.cpp`（16→8 与 g++ 一致） |
| 7 | `t = a > 3 ? 100 : 200` 得 1（静默错值） | `parse_expr` 把三元放在赋值**外层**，违反 [expr.cond] 优先级 | 新增 `parse_conditional` 层；赋值操作数改为 conditional-expression | `conditional_precedence.cpp` |
| 8 | `if (a < b && c > d)` 整条语句丢弃 | `<` 消歧只看尖括号能否闭合，不看闭合后 token | 按 [temp.names] 加入后继判据，并区分类型位置/表达式位置 | `relational_vs_template_id.cpp` |
| 9 | `int r = (d < 0.4);` 得 0 或垃圾 | sema 把比较结果类型标成操作数类型；SSA 重建 `infer_binop_type` 同错 → 下游补一条 `cvttsd2si` 盖掉已算好的 0/1 | 关系/相等/逻辑结果一律 `int`（与 C 前端同口径），并补 `CxxType::usual_arith_type` | `conditional_precedence.cpp`、`long_double_abi.cpp` |
| 10 | `long double` 报 `float with 80 bits`；字面量退化成 double（`1.0L/3.0L` 仅 53 位）；全局初值读回 0 | `lower_type` 只认 32/64；后缀被剥掉；静态初值发成 `F64` | 80 位接上 `Type::LongDouble`；`const_text::float_literal_bits` 让后缀承载类型；常量与全局初值走 `FloatConst::LongDouble` | `long_double_abi.cpp`（精度/布局/重载/数值迭代全一致） |
| 11 | `(a, b)` 逗号运算符解析失败 | 括号表达式未走 comma-expression | 括号内改用 `parse_comma_expr()`（[expr.comma]/1） | `conditional_precedence.cpp` |
| 12 | 局部 `struct S x;` / `union U u;` / `enum E e;` 整条语句丢弃 | elaborated type specifier 未把 tag 名计入类型部分 | `is_local_var_decl_start` 吞 tag 名并放行数组/花括号声明符 | `typedef_anonymous_aggregates.cpp`、`relational_vs_template_id.cpp` |
| 13 | 每个 native TU 冗余导出 `_ZTI/_ZTS` | 布局表里**每个类**都发 typeinfo | 改按需发射：多态类 + `typeid/throw/catch/dynamic_cast` 引用，再做基类传递闭包（不按名字特判） | `cpp_module_cross_tu_virtual_runtime` 等既有 RTTI/EH 夹具 |
| 14 | 缺缺陷发现仪器，进度只能靠"能不能编过" | — | `scripts/gcc_tree_cpp_probe.sh`（按组件、可 g++ 真值对照、错误频次排行） | — |

### 0.3 遗留缺陷与架构债务（本方案必须消化）

| ID | 问题 | 证据 | 严重度 |
|----|------|------|--------|
| B1 | ~~SSA→AST 结构化重建：分支的某条路径本身又是分支时，汇聚块之后的代码整体丢失~~ **已修** | 复验（重编后新二进制）：`if (a>10){...} else { if (a>3) {...} }` + 后续语句与 g++ 逐行一致（`x=7 y=3`）；后支配者 `compute_ipdoms` 与 `stop_at`/`visited` 侧收人同步修正 | 已关闭（保留回归夹具，见 Step 3） |
| B2 | ~~三元走 `InstKind::Select`：未选中分支也被求值；16 字节 `long double` 分支得 `nan`~~ **已修** | 复验：`w = a>3 ? (w1=11) : (w2=22)` 得 `w=11 w1=11 w2=0`；`c ? x : y`（long double）得 `0.5000`，均与 g++ 一致 | 已关闭（保留回归夹具，见 Step 4） |
| B3 | `__builtin_va_arg(ap, T)` 未贯通 native 管线，`<stdarg.h>` 变参函数体编不出来 | 复验：失败点已在 parser 层收窄——`s += __builtin_va_arg(ap,int)` 让整条 `for` 语句降级成 `OpaqueStmt`（调用实参位置放**类型**无法解析），而不是后端的 IR 缺失 | 高（libcpp 硬门） |
| B4 | 未声明的 tag 以指针/数组形式出现时（elaborated type specifier 引用不完整类型）报 `unknown class type` | libcpp 16/16 当前唯一阻塞：系统头里 `struct __locale_data *__locales[13];` 在全程没有一句 `struct __locale_data;` 前向声明 | 高（L0 关键路径） |
| B5 | native 仅剩两处依赖 legacy 回退的夹具，是删除 legacy 的**全部**前置 | 125 个 cpp 夹具中 2 个走回退：`enhanced_sfinae.cpp`（变参模板 `<typename...>`）、`g11_template_templates.cpp`（模板模板参数 `template<typename> class`）——均为 parser 层 `unsupported opaque declaration` | 中（阻塞 Step 2） |
| D1 | legacy 路径 `run_cpp_legacy_backend` 把降级出的 C 文本交给**宿主 `cc`** 编译（`E0504 host-cc failed`） | `src/driver.rs:2066,2164` | 架构诚实性：所有"legacy 编过"的结论无效 |
| D2 | 内置函数签名有**两套真源**：`builtin_decls_prelude` 用原型源文本，`builtins.rs::BUILTIN_LIBC_FUNCS` 用结构化 `BuiltinTy` | 两文件并存，无交叉校验 | 长期漂移 |
| D3 | 内置符号知识在 C 侧还有**第三套**：`src/parser.rs`/`src/preproc.rs` 的内联分支（如 `__builtin_va_list` 就地展开） | `src/parser.rs:6211` 与 prelude 各写一遍结构体 | 同上 |
| D4 | eager（前导无条件拼进每个 TU）与 lazy（查找失败才登记）两种注入策略未建模，靠"写在哪个文件"隐含 | eager 前导改变符号 id 分配，已迫使测试改为 `user_struct_count()` 排除内部类型 | 隐蔽错值来源 |
| D5 | 全局作用域自由函数不 mangle，且 native 主路径与模块路径已**分叉成两套链接名** | `int cfoo(int)` 调用 → dcc `U cfoo`，g++ → `U _Z4cfooi`；而 `mangle_global_function` 已被 `modules.rs`（`ModuleLinkagePlan`）与 `lower.rs` 使用 —— 即模块 TU 会用 mangled 名，主路径不会 | 阻塞 L2（与 libstdc++ 不可互链） |
| D6 | `builtin_decls_prelude` 文档承诺的 `every_declared_builtin_is_emitted_by_some_backend` 待确认存在；`is_va_builtin_name` 无消费方 | 模块头 §引用 vs 实际 tests | 文档失真 / 死接口 |

## 1. 目标分层与验收判据

"编译 gcc-15.3.0"不是单一目标，按依赖分层，每层是下一层的前置；**每层的判据都是"与 g++ 真值逐行一致"，不是"退出码 0"**。

| 层 | 目标 | 产物 | 硬门槛（缺口见 §3） |
|----|------|------|---------------------|
| L0 | native 后端走通 `libcpp/*.cc`（14 个真 TU） | `libcpp.a`（dcc 产物，非宿主 cc） | B1,B2,B3,D1 + 内建函数集合 + 模板/命名空间/重载基础 |
| L1 | 走通 gcc 主机生成器（`gcc/gen*.cc`） | `genmodes`/`genemit`/`gengtype` 等可运行 | L0 + 完整枚举/模板元编程惯用法 |
| L2 | 走通 `gcc/*.cc` middle-end（约 6M 行） | `cc1` | 真 libstdc++ 头解析 + 完整模板/SFINAE/两阶段查找 + 异常 + D5 |
| L3 | 走通 `gcc/cp/*.cc` | `cc1plus` | L2 + C++14 完备性 |
| L4 | `gcc -v` 正确显示版本且能编译 hello | 安装树 | libgcc/libstdc++ 运行时 + 驱动 |

结论（必须在验收时复述，避免"部分成功被读成成功"）：

1. L0/L1 是本方案的**可交付范围**，缺口是有限且可枚举的。
2. L2 及以上等价于实现一个符合 C++14 的完整前端（真 libstdc++ 头 + 模板实例化全套 + SFINAE + 两阶段名字查找 + 重载决议评分 + 异常表驱动展开 + RTTI + Itanium mangling）。dcc 现有 native 后端在 `CppFeature` 矩阵上只覆盖其中一部分（权威差集由 Step 0 脚本产出，不在本文写死数字），把它说成"修若干缺陷即可达成"不成立。
3. 因此本方案的验收口径是 **L0/L1 全绿 + L2+ 的缺口清单被逐项登记为夹具/Step**，而不是 `gcc -v`。

## 2. 架构：当前形状与目标形状

### 2.1 native 管线与内置符号注入（当前）

```
.cc/.cpp ─ preprocess(src/preproc.rs) ─ CppParser ─ CppDecl[]
                                              │
                          ┌───────────────────┴───────────────────┐
                          │ builtin_decls_prelude::decls()  ← eager │  内置类型 + 原型源文本
                          └───────────────────┬───────────────────┘
                                              ▼
                     Sema(名字查找/类型/HIR)  ←─ lazy: builtins::BUILTIN_LIBC_FUNCS
                                              │        (declare_builtin_if_known)
                     HIR ─ IrLowerer ─ SSA ─ rebuild_block ─ AST ─ codegen(x86_64/aarch64/riscv64)
                                                                ▲
                                             后端按名字拦截 __builtin_* (special/builtins.rs)
```

### 2.2 目标形状（消除 D2/D3/D4）

```
src/cpp/
├── builtin_symbols.rs            ← 唯一一张内置符号表 (是什么)
│     BuiltinSymbol { name, kind: Type|Func, signature: BuiltinSignature,
│                     injection: Eager|OnDemand, targets: &[Arch], abi_contract }
│     由 (name, BuiltinTy 结构化签名) 驱动, 不再用原型源文本表达签名
├── builtin_decls_prelude.rs      ← 只保留注入机制 (怎么进 TU)
│     decls() 由 builtin_symbols 生成 CppDecl, 文本只作为可读视图/单测对照
├── va_list_abi_contract.rs       ← ABI 耦合契约 (结构体名 + 字段偏移 + per-arch 形状)
│     与 src/parser.rs / src/ir.rs / 三后端 gen_va_start 的一致性测试住在这里
└── builtins.rs                   ← libc 函数表并入 builtin_symbols 的 OnDemand 分区

C 前端: src/parser.rs 的内联分支改为查询同一张表 (消除 D3)
legacy: 见 Step 2 (D1)
```

拆分判据（不是"类型 vs 函数"）：**是否携带 ABI 契约**（必须与后端逐字节同步）+ **注入策略**（eager 改变每 TU 的符号/栈槽账，lazy 只在需要时出现）。

## 3. 实施步骤

每步格式：问题 → 改动点 → 做法 → 新增/更新测试 → 交叉验证口径 → 完成判据。**不排时间，按依赖顺序串行推进。**

### Step 0 — 把"现状"变成可重跑的事实

- 问题: 本文与既有文档里的支持度数字来自人工统计，已出现互相矛盾。
- 改动点: 新增 `scripts/cpp_native_feature_gap_report.sh`。
- 做法: 遍历 `CppFeature::ALL`，对每项打印 `native_supported` / `end_to_end_supported`；对每个 probe 组件打印通过数与错误频次 top-N；输出一页纯文本报告到 `.work_tmp/`（不进仓库）。
- **强制前置: 报告脚本自先 `cargo build -j2 --release`（或校验 `target/release/dcc` 比 `src/**.rs` 新）再跑 probe**。否则错误队列基于陈旧二进制，会直接误导优先级排序（本文已发生过一次，见 §0.1 说明二）。
- 测试: 脚本自身 `bash -n` + 一次 dry-run；并验证"源码比二进制新"时确实触发重编。
- 完成判据: 报告可一键复跑，且本文 §0/§1 的每个数字都能由它复现。

### Step 1 — 不完整 tag 的前向引用（当前 libcpp 唯一阻塞 B4）+ 内建函数集合收尾

- 问题: 复验后 16/16 TU 统一卡在 `unknown class type: __locale_data`；来由是系统头里的
  `struct __locale_data *__locales[13];` —— **全局没有一句 `struct __locale_data;` 前向声明**。
  （前一站 `__builtin_bswap*` 已被 `BUILTIN_FUNC_DECLS` 消化；修正一个事实：它不是来自
  `<sys/byteorder.h>`（glibc 无此头），而是 `<byteswap.h>`→`bits/byteswap.h` 里
  `__bswap_16()` 的**函数体**引用 `__builtin_bswap16`，所以必须在"系统头函数体内被触发"这个形状下验，而不是在用户调用点。）
- 改动点: `src/cpp/parser.rs`（类型位置的 elaborated specifier）、`src/cpp/sema.rs`（`convert_type` 的未知类名路径与不完整类型登记）。
- 做法:
  1. 将「`struct|union|enum <Tag>` 作指针/引用/数组元素出现且 Tag 从未声明」建模成**不完整类符号**
     （Itanium/C++ 允许指向不完整类型的指针，[basic.types]），而不是当用户类去要求定义；
     现有 `Typedef{Class(X),X}` 前向声明路径已有这个形状，复用而不是另开名字特例；
  2. 不完整类型的 `sizeof` / 成员访问必须仍报硬错（不能因为"登记了"就默认 0 尺寸默默过）；
  3. 落实 D6：补齐"表里每个名字必须被某个后端拦截"的一致性测试（模块头已承诺但尚未实现），并删除无消费方的 `is_va_builtin_name` 或把它接进诊断。
- 测试: `tests/fixtures/cpp_abi_c_style_decls/incomplete_tag_pointer.cpp`（指针成员、函数形参、跨结构体引用，外加一条"对不完整类型取 sizeof 必须报错"的负向用例）。
- 交叉验证: 与 g++ 同夹具比 stdout 全行 + 比退出码；负向用例比 gcc 的诊断行为。
- 完成判据: `--component=libcpp` 首错不再是 `__locale_data`，也不得出现"本该报错的不完整类型使用却编过了"。

### Step 2 — 让 legacy 路径不再借宿主编译器（消除 D1）

- 问题: `run_cpp_legacy_backend` 把 C 文本交给 `cc -c`，等于"用 gcc 编译 dcc 的产物"。
- 改动点: `src/driver.rs:2066`、`src/cpp/legacy/*`。
- 做法: 二选一并保持全局一致 —— (a) 把降级出的 C 文本送回 **dcc 自己的 C 管线**（进程内），或 (b) 直接删除 legacy 路径，让 native 成为唯一后端（`docs/cxx-native-foundation-fix-plan.md` 已把这列为前置）。优先 (b)，因为 (a) 会长期保留两套真源。
- **前置 = B5（已量化，只剩两条）**: 125 个 cpp 夹具中仅 `enhanced_sfinae.cpp`（变参模板 `<typename...>`）与 `g11_template_templates.cpp`（模板模板参数 `template<typename> class Container`）仍靠回退，均为 parser 层 `unsupported opaque declaration`。先把这两条文法接进 native，再删 legacy。
- 测试: 现有 `cpp_*` 夹具若因删除 legacy 而失败，逐条转为 native 缺陷登记（进 Step 3+），**不允许**保留回退来掩盖。
- 完成判据: 仓库里不存在"C++ 产物由宿主 cc 生成"的代码路径；`libcpp.a` 的 `.o` 全部可由 `readelf` 证明出自 dcc。

### Step 3 — B1 回归护栏（已修，补夹具与注释清理）

- 状态: **已完成**（复验：`if/else{if}` 及后续语句与 g++ 逐行一致；`compute_ipdoms` 已接入 `branch_join` 并有单测 `src/ssa.rs:5147`）。
- 问题: 一条路径本身是分支时，汇聚块之后的语句整体消失（静默丢代码）。
- 做法（已落地）: `walk_to_join` 启发式失配时回落到分支块的直接后支配者 `compute_ipdoms`。
- 待收作（本步唯一余项）: 新增 `tests/fixtures/cpp_abi_c_style_decls/nested_branch_code_loss.cpp`，把当时的手测形状固定成回归夹具——覆盖 `if/else{if}`、`else if` 链、`do{...}while` 内的 `&&`、三元与短路混合；每条路径打印自己的计数器，丢代码直接表现为缺行。
- 交叉验证: 与 g++ 比 stdout 全行 + 退出码。
- 完成判据: 夹具入组并全绿（`cpp_abi_groups_are_all_covered` 会自动强制登记）。

### Step 4 — B2 回归护栏（已修，验证残留注释已清）

- 状态: **已完成**（复验：`w = a>3 ? (w1=11) : (w2=22)` 得 `w2=0`；`long double` 三目得正确值，均与 g++ 一致）。
- 做法（已落地）: `HirExprKind::Conditional` 已改走 `lower_conditional`（控制流 + 结果变量合并），`InstKind::Select` 旁路已删除，未留两种实现并存；旧注释里的"已知限制/未修缺口"段已消失。
- 待收作: 把 Step 0 复验用的两个形状（未选分支副作用计数器、`long double` 三目）写回 `conditional_precedence.cpp` / `long_double_abi.cpp` 夹具（假设驱动期间曾被删过则补上）。
- 完成判据: 上述夹具含未选分支计数器与 `long double` 三目行且全绿。

### Step 5 — 修 B3：`__builtin_va_arg` 贯通 native 管线

- 问题（复验后收窄）: 当前失败发生在 **parser 层**，不是 IR 层缺 `Expr::VaArg`——`s += __builtin_va_arg(ap,int)` 让整条 `for` 语句降级成 `OpaqueStmt`，原因是调用实参位置出现**类型**（`int`）无法解析。
- 改动点: `src/cpp/parser.rs`（类型实参，对齐 C 前端 `parse_postfix_suffix` 对 `__builtin_va_arg` 的专处理）、`sema.rs`（va_arg 结果类型 = 请求类型）、`hir.rs`、`ssa.rs`（新增 `InstKind::VaArg`）、`lower_ir.rs`（重建为 `Expr::VaArg`）。
- 做法: 复用 C 侧既有 ABI 实现，**绝不在前端重写 SysV 取参**：AST 产出 `Expr::VaArg{ap,ty}`，typed IR 由 `ir.rs` 既有映射转 `ExprKind::VaArg`（aarch64/riscv64 已消费），x86_64 走 `emit/expr/va_arg*.rs`。`InstKind` 新变体的全部受影响分支由编译器穷尽性检查列出，逐个补齐（含 `lto_bitcode`、`ssa_passes`）。Step 3/4 已完成，本步**不再被它们阻塞**。
- 测试: `tests/fixtures/cpp_abi_c_style_decls/va_arg_int_float_ptr.cpp`：整数/浮点/指针/结构体/`long double`/`__int128` 混合取参、跨函数传 `va_list*`、`va_copy`。
- 交叉验证: 与 g++ 逐行比 stdout（项目已有 va_arg ABI 陷阱记忆：字节来源轴与值表示轴必须分离，夹具要同时压这两轴）。
- 完成判据: `--component=libcpp` 首次出现非零通过数，且 `errors.cc`/`directives.cc` 走通。

### Step 6 — 内置符号表合一（消除 D2/D3/D4）

- 改动点: 新增 `builtin_symbols.rs`、`va_list_abi_contract.rs`；改写 `builtin_decls_prelude.rs`、`builtins.rs`、`src/parser.rs`/`src/preproc.rs` 的内联分支。
- 做法: 按 §2.2 落地；`injection` 成为数据字段；`signature` 统一为结构化 `BuiltinTy`；C 侧改为查同一张表；per-arch 形状（x86_64 SysV / aarch64 / riscv64）留在契约模块。
- 测试: 契约测试集中在 `va_list_abi_contract.rs`（名字/偏移/与后端拦截集合双向一致）；表加一条"每条目都有 kind 与注入策略"的结构校验。
- 完成判据: 全仓仅一处定义内置签名；`prelude_source()` 不再有手写原型文本。

### Step 7 — 链接兼容性：自由函数 Itanium mangling（D5）

- 问题: 全局自由函数不 mangle（`cfoo` vs `_Z4cfooi`），与 g++ 产物/libstdc++ 无法互链，是 L2 的前置。
- 改动点: `src/cpp/lower_ir.rs::symbol_link_name*`（唯一发射点）、`sema::compute_linkage_name`、`mangle.rs`（已有 `mangle_global_function` 等未被 native 使用）。
- 做法: 以 `symbol_link_name` 为唯一定义点切到真 Itanium 编码；`extern "C"` 需带链接性标志（Step 1/§0.2#5 目前靠"dcc 不 mangle"巧合正确，必须先建模 linkage 再切 mangle）；`main`/`operator new`/libc 声明等保持 C 链接。
- 测试: 新增分组 `tests/fixtures/cpp_abi_mangling/`：跨 TU 调用（dcc TU + g++ TU 互链）、重载集、模板函数、`extern "C"` 与块内/块外一致性。
- 交叉验证: 不只比 stdout，还要 `nm -C` 比对两边符号集合。
- 完成判据: dcc 的 `.o` 能被 g++ 直接链进 libstdc++ 程序，反之亦然。

### Step 8 — 冲 L0：`libcpp/*.cc` 全量 native

- 前置: Step 1–5。
- 做法: 以 probe 的 top-1 错误为唯一优先级队列，逐条"缺陷 → 修复 → 夹具 → 交叉验证"循环，直至 14 个真 TU 全部 native 通过并 `ar` 成 `libcpp.a`（Step 2 后必为 dcc 产物）。
- 完成判据: `gcc_tree_cpp_probe.sh --component=libcpp` 通过数 = 14（`makeucnid`/`makeuname2c` 是构建期表生成器，单独归类），且 `libcpp.a` 链出的工具可运行。

### Step 9 — 冲 L1：gcc 主机生成器

- 前置: Step 8。
- 做法: `--component=generator` 入队；`gengtype` 相关失败按 `docs/gcc_build_progress.md` 里记录过的"降级产物语法不完整"逐项在 native 侧复现并修（不再回到文本剥离）。
- 完成判据: `genhooks/gengenrtl/genchecksum/genmodes/gengtype` 由 dcc 编译并成功运行产出预期生成文件。

## 4. 验证基础设施

| 工具 | 用途 | 关键约束 |
|------|------|----------|
| `scripts/gcc_tree_cpp_probe.sh` | 按组件批量编译 GCC 树 C++ TU，聚合错误频次；`--compiler=g++` 做真值对照 | 必须 `JOBS=2 nice -n 10` |
| `scripts/cpp_abi_gcc_cross_check.sh` / `tests/cpp_abi_gcc_cross_check.rs` | 夹具级 stdout+退出码逐行比对；新分组由 `cpp_abi_groups_are_all_covered` 强制登记 | 分组新增必须同时加 cargo 测试 |
| `scripts/dcc_gcc_diff.sh` | 单 TU 的 dcc/gcc 产物差分 | — |
| `tools/diff_test`、`gdb` | 指令级差异与运行期定位 | — |

夹具约定（沿用现状）：自己 `printf` 出全部观察值，不靠退出码编码，否则"算错但没触发断言"会被放过。

## 5. 影响范围

- 前端: `src/cpp/{parser,sema,hir,lower_ir,native,abi_layout,mangle,feature_matrix,builtin_*}.rs`
- SSA: `src/ssa.rs`、`src/ssa_passes.rs`（B1/Step 5 触及 `InstKind` 穷尽匹配）
- C 侧: `src/parser.rs`、`src/preproc.rs`、`src/ir.rs`（D3 合一）
- 后端: `src/codegen/{x86_64,aarch64,riscv64}/special/builtins.rs`（Step 1/5 拦截集合）
- 驱动: `src/driver.rs`、`src/cpp/{driver,legacy/*}.rs`（Step 2 删/改 legacy）
- 测试: `tests/fixtures/cpp_abi_c_style_decls/`（新）、`cpp_abi_mangling/`（Step 7 新增）、`tests/integration/*`、`docs/gcc_build_*.md` 需按新事实重写"libcpp 完成度"表述

## 6. 下一步与依赖

```
Step 0 (事实基线)
   └─ Step 1 (内建函数集合)  ─┐
        Step 2 (去宿主 cc)   ─┼─ Step 3 (B1 丢代码) ─ Step 4 (B2 三元) ─ Step 5 (B3 va_arg)
                              │                                        └─ Step 8 (L0) ─ Step 9 (L1)
                              └─ Step 6 (内置符号表合一)   Step 7 (mangling) ──┘（L2 前置）
```

- 立即可并行启动: Step 0、Step 1、Step 2、Step 6（互不冲突的文件面）。
- 硬串行: Step 3 → Step 4 → Step 5 → Step 8 → Step 9。
- Step 7 可与 Step 6 并行，但必须在 Step 8 之后验收（跨 TU 链接用例依赖 native 能编出 libcpp）。
