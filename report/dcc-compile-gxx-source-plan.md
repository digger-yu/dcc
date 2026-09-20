# dcc 编译 g++ 源码特性补齐方案

- 状态: 提案
- 前置: [cpp_native_backend_implementation_plan.md](./cpp_native_backend_implementation_plan.md) 定义的 Stage 1-10 完成
- 目标源码: `/root/gcc-15.3.0`（GCC 15.3.0，含 libstdc++-v3 与 gcc/cp）
- 约束: 本方案仅给出特性缺口与实施路线，不涉及代码修改

## 0. 术语与目标分层

"编译 g++ 源码"并非单一目标，按依赖关系分为四个层级。每一层是下一层的前置。

| 层级 | 目标 | 代表产物 | 规模 | 难度 |
|------|------|----------|------|------|
| L0 | 解析并编译 **libstdc++ 头文件**（header-only 部分） | 任何 `#include <vector>` 的用户代码走 dcc 编译 | 80 标准头 + 160 bits 头 | 高（语法/模板门槛最高） |
| L1 | 编译 **libstdc++ 运行时源码** | `src/*.cc`（100 文件 / 812,928 行） | 大 | 中高（依赖系统接口） |
| L2 | 编译 **libsupc++**（异常/RTTI/guard 运行时） | `__cxa_*` 符号、typeinfo | 90 文件 | 中 |
| L3 | 编译 **gcc/cp**（g++ 前端本体） | cc1plus | 41 .cc / 8,152,868 行 | 极高（规模 + 前置工具链） |

结论先行:

1. **L0 是硬需求**——dcc 若要编译真实 C++ 程序，迟早要解析真 libstdc++ 头（当前 dcc 使用自带的最小 stdlib，见 `src/cpp/stdlib/headers/`，约 50 个自实现头，非真 libstdc++）。
2. **L1/L2 建议合并实施**——libstdc++ 运行时强依赖 libsupc++ 的 `__cxa_*` 与真 Itanium ABI 实现，两者必须同时成立。
3. **L3 语法门槛反而低于 L0**——gcc/cp 以 C++98/03 风格为主（宏体系 + union + 手写容器），C++20 特性仅 39 处 concept/requires；真正的障碍是 870 万行规模、依赖 gengtype 预生成 `gt-cp-*.h`、依赖 `gcc/` 顶层基础设施头（tree.h/vec.h 等）。**建议 L3 延后**，作为 L0-L2 全部通过后的独立长期项目。

既有相关工作（M26.6 里程碑）已记录在 [gcc_build_guide.md](./gcc_build_guide.md) 与 [gcc_build_progress.md](./gcc_build_progress.md)：libiberty/libcpp/libcody 已可用 dcc 编译，gengtype/genmodes 等工具仍失败。本方案与 M26.6 不同，聚焦"C++ 语言特性补齐"而非"全树构建脚本"。

## 1. 现状盘点（dcc vs 需求差距）

### 1.1 dcc C++ 双路径架构

dcc 有两条并行 C++ 路径（[feature_matrix.rs](../src/cpp/feature_matrix.rs)）:

- **legacy 路径**: C++ AST → C 文本降级（`src/cpp/lower.rs`，12657 行）→ 复用 C 全链路。大量"支持"是文本替换 hack。
- **native 路径**: C++ AST → sema → HIR → abi_layout → typed IR（`src/cpp/native.rs`/`lower_ir.rs`）。当前 Stage 3 基础阶段。

默认 `CxxNativeMode::Auto`。native 支持 19 项特性（含模板单态化、SJLJ EH、简化 RTTI），另有 10 项仅 legacy 支持（PartialSpecialization、Namespace、OperatorOverload、NewDelete、InitializerList、StaticInitGuard、Coroutine、Concept、Module）。

### 1.2 与 libstdc++ 需求的关键差距（速查表）

| 需求（来自 libstdc++ 统计） | dcc 现状 | 缺口 |
|---|---|---|
| concept / requires 子句 | parser 能解析 concept 声明；无 requires 表达式语义 | 高 |
| if constexpr（127 处） | 解析支持；求值失败降级为运行时 if | 高 |
| 可变参数模板 + 包展开 | 仅解析容忍 `...`，无展开语义 | 高 |
| 全特化 / 函数模板重载 | 无 | 高 |
| 模板模板参数（11 处） | 无 | 高 |
| 折叠表达式（2 处） | 无 | 中 |
| constexpr 变量模板（type_traits 的 `_v`） | 无 | 高 |
| SFINAE（enable_if / void_t） | 字符串替换级，非类型系统级 | 高 |
| 两阶段名字查找 | 无（模板是文本替换） | 高 |
| 重载决议（转换序列评分） | 无（只合并候选集） | 高 |
| ADL | 无 | 中 |
| 访问控制 / friend | 未实施（注释明说暂不跟踪） | 中 |
| 结构化绑定（15 处） | 无 | 中 |
| 尾置返回类型 | 无 | 中 |
| 用户定义字面量（18 处） | 无 | 中 |
| raw string / 字符串前缀 | 无（`R"(` 无法识别） | 中 |
| static_assert 语义（148 处） | 仅关键字剥离，无求值 | 中 |
| inline namespace | 无 | 中 |
| extern template | 无 | 中 |
| consteval（13 处） | 关键字有，无语义 | 中 |
| `__builtin_*`（68 处） | C++ 路径无支持表（仅 3 处特例） | 高 |
| 类型 trait 内建（`__is_base_of` 等） | 无 | 高 |
| `__atomic_*`/`__sync_*`（357 处） | C 路径完整，C++ 路径依赖透传 | 中 |
| `__int128`（26 处） | 词法级 token 有；codegen 依赖 C 路径 | 中 |
| `__thread` TLS | 需确认 C 后端 TLS 支持 | 中 |
| `__cxa_*` / 真 Itanium ABI | 自实现简化版（SJLJ EH、name-only typeinfo、简化 dynamic_cast） | 高 |
| `[[no_unique_address]]` 等属性 | 全部剥离（仅保留 weak/nothrow）→ ABI 布局错误风险 | 高 |
| `#pragma GCC system_header` | 无特殊处理 | 低 |
| 内联汇编 | 需求为 0（gcc/cp 与 libstdc++ 均无） | 不需要 |

### 1.3 libstdc++ 特性用量统计（证据基线，GCC 15.3.0）

| 特性 | 数量 | 分布 |
|---|---|---|
| `if constexpr` | 127 处 / 15 文件 | ranges_base.h 44、hashtable.h 30 |
| `consteval` | 13 处 / 5 文件 | chrono_io.h、unicode.h |
| `__builtin_*` | 68 处 / 15 文件 | basic_string.h 9、random.h 7 |
| `decltype(auto)` | 17 处 / 6 文件 | stl_iterator.h 7 |
| `__int128` | 26 处 / 6 文件 | iterator_concepts.h 9 |
| `__atomic_`/`__sync_` | 357 处 / 10 文件 | atomic_base.h 286 |
| UDL `operator""` | 18 处 | chrono.h 13、basic_string.h 5 |
| 结构化绑定 | 15 处 / 5 文件 | ranges_algobase.h 7 |
| 模板模板参数 | 11 处 / 5 文件 | iterator_concepts.h 4、ptr_traits.h 3 |
| 折叠表达式 | 2 处 | uses_allocator.h、iterator_concepts.h |
| static_assert | 148 处 / 40 文件 | random.h 42、hashtable.h 11 |
| 虚继承 | 1 处 | ostream.h |
| 内联汇编 | 0 处 | — |

## 2. 特性缺口清单与解决方案（按层组织）

缺口按"实现层"分组。每项给出: 现状 → 方案 → 依据（gcc 源码位置）。

### 2.1 词法层（lexer.rs）

**G1. Raw string 字面量 `R"delim(...)delim"`**
- 现状: 字符串扫描（lexer.rs L626-654）到匹配引号即止，`R"(...)"` 会在 `R` 后 `"` 处切断。
- 方案: 新增 `lex_raw_string()`。识别 `R` 前缀后读取可选定界符（<=16 字符、非空白非括号非斜杠），从 `(delim"` 扫到 `)"delim`。token 值保留原始文本（codegen 原样输出），同时记录去定界符后的内容供字符串处理。
- 依据: libstdc++ 头文件中有少量 raw string（如 version.h、format 相关测试用），但 g++ 源码的 `__cxa_demangle` 测试等大量使用。

**G2. 编码前缀 `L`/`u8`/`u`/`U` 字符串与字符**
- 现状: 前缀字符被当作普通标识符（`u8"..."` 会解析为标识符 `u8` 后跟字符串）。
- 方案: lexer 在 `L/u8/u/U` 后紧跟引号时合并为一个 token 并记录前缀；codegen 原样输出前缀（C 路径已支持宽字符/UTF-8 字符串）。字符字面量 `u8'x'` 同理。
- 依据: `bits/char_traits.h`、`std/string` 的 `operator""s` 相关、`u8"..."` 在 format/unicode.h 中出现。

**G3. 用户定义字面量（UDL）**
- 现状: 无。
- 方案: 分两半实施——
  1. **lexer/parser**: 数字/字符串字面量后跟标识符时切分为 `Literal + Suffix`；解析 `operator""` 声明（raw/cooked/数字三种形式）。
  2. **sema**: 在字面量后缀所在命名空间做 ADL 查找 UDL 运算符，按标准选择规则（整型/浮点/字符串/字符/cooked/raw）生成调用。libstdc++ 仅 18 处且集中在 chrono（`operator""h/min/s`）与 string（`operator""s`），可先只支持 cooked string 与整型形式。
- 依据: `std/chrono` 的 `operator""h` 等 13 处、`bits/basic_string.h` 的 `operator""s` 5 处。

**G4. 数字分隔符 `1'000'000`（C++14）**
- 现状: `'` 在数字中无处理。
- 方案: lexer 数字扫描时跳过单引号（校验两侧为数字）。

### 2.2 语法层（parser.rs）

**G5. 结构化绑定 `auto [a, b] = expr;`**
- 现状: 无。
- 方案: 解析 init-declarator 时识别 `[` → `StructuredBinding { names, init }`。sema 展开:
  - 聚合/数组: 绑定到隐藏临时对象的成员/元素；
  - tuple-like: 绑定到 `get<N>(t)` 的引用（`tuple_size`/`tuple_element`）。
  legacy 降级为: 生成 `__dcc_binding0/1` 等隐藏变量 + 引用赋值。
- 依据: ranges_algobase.h 7 处、basic_string.h 3 处。

**G6. 折叠表达式 `(args + ...)`**
- 现状: 无。
- 方案: parser 识别二元折叠（左/右/一元）与 `...` 位置；语义展开: 一元折叠 = `e1 op e2 op ... op eN`，二元折叠 = `(e1 op ... op eN) op init`。**依赖 G12（包展开）先行**。
- 依据: uses_allocator.h 1 处、iterator_concepts.h 1 处（量少，可后置）。

**G7. 尾置返回类型 `auto f() -> T`**
- 现状: 无。
- 方案: 函数声明符解析到 `->` 时继续解析 trailing-return-type，存入函数签名；sema 以此作为返回类型（`decltype(auto)` 需在函数体求值后推导）。
- 依据: libstdc++ 用尾置返回的 `decltype(auto)` 17 处（stl_iterator.h）。

**G8. static_assert 语义**
- 现状: lower.rs L6532 仅剥离关键字，无求值。
- 方案: parser 解析 `static_assert(cond)`/`static_assert(cond, msg)`；接入常量求值（见 2.5），条件为常量 `false` 时报编译错误（含可选 msg）。**libstdc++ 148 处，属于编译 libstdc++ 头的基本门槛，优先级高**。
- 依据: random.h 42、hashtable.h 11。

**G9. inline namespace**
- 现状: 无。
- 方案: parser 解析 `inline namespace ns {}`；sema 的 `lookup_unqualified`/`lookup_qualified` 需把 inline 命名空间的成员提升到外层作用域参与查找。libstdc++ 用 `inline namespace __cxx11`（ABI 版本命名空间，`_GLIBCXX_USE_CXX11_ABI`）与 `__detail`。
- 依据: `bits/c++config.h` 的 `_GLIBCXX_BEGIN_NAMESPACE_VERSION` 宏展开为 inline namespace。

**G10. extern template**
- 现状: 无。
- 方案: parser 识别 `extern template` 声明；语义: 抑制隐式实例化（该实例化在显式实例化 TU 中已存在）。libstdc++ 的 `src/c++98/misc-inst.cc`、`src/c++17/string-inst.cc` 是显式实例化源；头文件内大量 `extern template class basic_string<char>;`。
- 依据: `include/std/string` 尾部 extern template 块。

**G11. 模板模板参数 `template<template<class> class C>`**
- 现状: 无。
- 方案: 模板参数列表支持嵌套 `template<...> class/typename`；template.rs 的 `resolve_args` 允许模板实参为另一个模板（绑定到模板参数）。依赖 G14 的语义实例化。
- 依据: iterator_concepts.h 4、ptr_traits.h 3、alloc_traits.h 2。

**G12. 可变参数模板 + 包展开（模板层核心）**
- 现状: `typename... Args` 仅解析容忍，lower 时忽略 `...`。
- 方案: 这是模板系统的最大工程，与 G14 合并实施——
  1. 表示: 参数包存储为包类型列表；`sizeof...(Args)` 求包长度；
  2. 展开位置: 模板实参列表、函数实参列表、初始化列表（`T{args...}`）、基类列表、`std::tuple<Args...>` 等；
  3. 包推导: 实参列表与包参数匹配（前缀匹配 + 尾部包）。
  4. 折叠表达式（G6）与 `std::index_sequence` 模式依赖此能力。
- 依据: libstdc++ 全面依赖包（tuple、variant、common_type、conjunction/disjunction、`make_index_sequence`）。

**G13. 全特化 `template<>`**
- 现状: 仅偏特化排序（template.rs L281），无 `template<>` 专特化。
- 方案: 解析 `template<>` + 具体实参的声明为显式特化；实例化查找时优先匹配显式特化 > 主模板 > 偏特化。
- 依据: `allocator<void>`（allocator.h L75）、`default_delete<_Tp[]>`（unique_ptr.h L105）、type_traits 大量特化。

**G14. 语义级模板实例化（替代文本替换单态化）**
- 现状: template.rs 是"AST 文本替换"：`type_key()` 生成 `__dcc_template_Box_int` 类名，`try_substitute` 逐字符替换。无真实类型系统绑定。
- 方案（两条路线，建议渐进）:
  - **路线 A（推荐，渐进）**: 保留文本替换外壳，但把"依赖名解析"接入 sema——实例化时对替换后的声明做一次 sema 名字查找；新增全特化表（G13）、包展开（G12）、SFINAE 失败即回退（不报错）。
  - **路线 B（彻底）**: 类型系统级实例化（GCC/Clang 模型：`tsubst`）。sema 维护 `TemplateDecl` 参数→实参绑定，实例化时构造真实 `CxxType`。工程量最大，但只有路线 B 能正确支撑重载决议（G20）、ADL（G21）、两阶段查找（G15）。
- 决策建议: 先走路线 A 打通 libstdc++ 子集，再视需求迁移路线 B。

**G15. 两阶段名字查找**
- 现状: 无（模板文本替换阶段不区分依赖/非依赖名）。
- 方案: 模板定义解析时对非依赖名立即绑定；依赖名（依赖模板参数的类型/表达式）延迟到实例化点绑定。libstdc++ 头大量依赖此语义（`typename` 依赖类型、`_Tp::value_type` 等）。

**G16. SFINAE 类型系统级**
- 现状: feature_degrade.rs 是 `SfinaeCtx` 字符串替换（bindings 是 `Vec<(String,String)>`），非类型系统级。
- 方案: 模板实参替换失败（enable_if 中无 `::type`、void_t 展开失败、requires 约束不满足）应使该候选不可行而非报错；重载集合中过滤失败候选后继续决议。**这是 libstdc++ 算法/迭代器头大量依赖的机制**。
- 依据: unique_ptr.h L121 `_Require<is_convertible<...>>`、stl_vector.h L145-147 构造函数 requires 子句。

**G17. constexpr 变量模板 `_v`**
- 现状: 无。
- 方案: 模板变量声明 + 实例化（`template<typename T> constexpr bool is_integral_v = is_integral<T>::value;`）。依赖 G14。
- 依据: type_traits 的 `_v` 变量模板族（is_integral_v 等，数百个）。

**G18. 模板实参推导完备化**
- 现状: `deduce_args`/`collect_bindings` 只做结构匹配（Pointer/Reference/Template）。
- 方案: 补齐标准推导规则——引用折叠（`T&&` + 实参类型）、数组/函数指针衰减、`const` 剥离、模板模板参数、参数包推导、推导上下文忽略（`typename T::foo` 不参与推导）。**配合 G20 重载决议使用**。
- 依据: `std::forward`/`std::move`（bits/move.h）、通用引用在 libstdc++ 头中普遍。

**G19. 属性语义保留（ABI 关键）**
- 现状: lower.rs L4851-4853 剥离全部 `__attribute__((...))`，仅保留 weak/nothrow。
- 方案: 建立属性白名单并逐项语义化，优先级排序:
  1. `[[no_unique_address]]`/`__attribute__((__no_unique_address__))`（`_GLIBCXX_EMPTY_BASES` 宏）→ **影响空基类优化与布局，必须进入 abi_layout**，否则 dcc 类布局与 g++ 不兼容;
  2. `__attribute__((__always_inline__))`（allocator.h 11/166 处）→ 映射为 inline 语义;
  3. `[[nodiscard]]`/`[[unlikely]]`/`[[maybe_unused]]`/`[[noreturn]]` → 解析并忽略（不影响语义）;
  4. `__attribute__((__weak__))`/`visibility` → 符号属性（native 路径需输出到汇编）。
  5. `#pragma GCC system_header`/`diagnostic push/ignored/pop` → parser 吞掉不报 warning（180 处 pragma，libstdc++ 头必现）。
- 依据: `bits/c++config` 的 `_GLIBCXX_PURE/CONST/NORETURN/NOTHROW/VISIBILITY` 宏体系、allocator.h、`[[no_unique_address]]` 在 tuple/optional 的 EBO 应用。

### 2.3 语义层（sema.rs）

**G20. 重载决议**
- 现状: 无转换序列评分（Grep `resolve_call|best_match|rank|viable` 零结果），overload set 只合并候选。
- 方案: 实现候选函数集合（成员/非成员、模板/非模板、构造函数、转换函数）+ 可行性检查（实参数量、隐式转换、默认实参、参数包）+ 转换序列评级（精确 > 提升 > 标准转换 > 用户定义转换 > 省略号）+ 模板特化偏序。**这是编译 libstdc++ 算法头（sort/find 等 3 万行 stl_algo.h）的必要条件**。
- 依据: bits/stl_algo.h、bits/algorithmfwd.h 依赖重载决议选择迭代器标签（iterator_category 分派）。

**G21. ADL（实参依赖查找）**
- 现状: 无。
- 方案: 函数调用失败于普通查找后，收集实参类型关联的命名空间（类类型所在命名空间、模板实参类、基类）继续查找。UDL（G3）、`swap` 惯用法、`operator<<` 重载依赖 ADL。
- 依据: `std::swap` 的 ADL 惯用法在 libstdc++ 头中广泛使用。

**G22. 访问控制与 friend**
- 现状: sema.rs L2086 注释明说"默认 private，暂不跟踪 access specifier"。
- 方案: 实现访问控制检查（private/protected/public + 派生类访问规则）+ friend 声明（类内 `friend` 函数/类/模板）。libstdc++ 头大量使用 private 实现细节 + friend 注入。
- 依据: 各容器头的 `_Vector_impl` 私有嵌套类型、iterator 的 friend 注入。

**G23. 引用折叠与完美转发**
- 现状: 右值引用类型存在（CxxType::RValueRef），折叠规则无。
- 方案: `T&&` + `T&`/`T&&` 实参的折叠（`& &`→`&`、`& &&`→`&`、`&& &`→`&`、`&& &&`→`&&`）；`std::forward<T>` 依赖 `T` 推导 + `static_cast<T&&>`。配合 G18。
- 依据: bits/move.h 的 `std::forward`。

**G24. 类型 trait 内建（`__is_*` 族）**
- 现状: 无。
- 方案: 在 sema 对完整类型信息实现 GCC 内建 trait:
  - 简单型: `__is_same`、`__is_enum`、`__is_union`、`__is_class`、`__is_empty`、`__is_polymorphic`、`__is_final`、`__is_aggregate`、`__is_abstract`、`__is_literal_type`、`__is_standard_layout`、`__is_trivial`、`__is_trivially_*`（构造/拷贝/移动/析构/赋值）、`__is_nothrow_*`（依赖 noexcept 分析）、`__is_assignable`、`__is_constructible`、`__is_convertible`、`__is_invocable`、`__is_same_as`、`__is_scoped_enum`;
  - 复杂型: `__is_base_of`（继承图）、`__is_convertible`（调用转换分析，依赖 G20）。
  libstdc++ 的 `type_traits`（122k 行）整个建立在 `__is_*` 内建之上，**属于 L0 的硬前置**。
- 依据: include/std/type_traits、bits/cpp_type_traits.h。

### 2.4 常量求值（cpp_const_eval.rs）

**G25. constexpr 函数完整求值**
- 现状: cpp_const_eval.rs 仅 183 行，支持字面量/一元/二元/三元/`sizeof`(5 种内置类型)/`noexcept`，无循环、无函数调用、无 new/delete、无字符串。
- 方案: 将常量求值从"表达式求值"升级为"解释器"——
  1. 支持循环/分支/局部变量/块作用域（在 HIR/SSA 上做符号执行，或独立 AST 解释器）；
  2. 支持 constexpr 函数调用（调用图 + 递归深度限制）；
  3. 支持 `sizeof(任意类型/表达式)`、`alignof`、`typeid`（常量上下文）;
  4. 支持 constexpr 类对象、`constexpr std::array` 等字面类型;
  5. C++20 起支持 constexpr 内 `new`/`delete`（libstdc++ 的 constexpr vector/string 路径依赖，stl_vector.h L209 的 `std::is_constant_evaluated()` 分派）。
- 依据: bits/stl_vector.h L209、bits/basic_string.h 的 constexpr 分支。

**G26. if constexpr 语义化**
- 现状: 求值失败降级为运行时 if-else 两分支保留（lower.rs L10416-10429）。
- 方案: 条件为常量时，被丢弃分支必须不参与实例化（丢弃分支内的错误不报）。**这对模板头是语义要求而非优化**。
- 依据: ranges_base.h 44 处 if constexpr。

**G27. consteval / constinit**
- 现状: 关键字有，语义无。
- 方案: consteval 函数强制编译期求值（否则报错）；constinit 检查初始化常量性。libstdc++ 13 处 consteval（chrono_io.h、unicode.h）。
- 依据: chrono_io.h、unicode.h。

**G28. `__builtin_is_constant_evaluated`**
- 现状: cpp_const_eval 无折叠。
- 方案: 常量求值上下文中返回 true、运行时返回 false；`std::is_constant_evaluated()`（libstdc++ 用内建实现）据此分派 constexpr 路径。
- 依据: bits/stl_vector.h L209。

### 2.5 内建函数与扩展

**G29. `__builtin_*` C++ 路径支持表**
- 现状: C++ 路径仅 3 处特例（SSE 文本替换 lower.rs L4761、va_list typedef L7730、关键字剥离）。C 路径 codegen 已支持 100+ 内建（support_features.md L140-210）。
- 方案: 在 cpp 侧建 `__builtin_*` 映射表，复用 C codegen。libstdc++ 用到的 68 处主要清单:
  - 控制: `__builtin_expect`/`__builtin_expect_with_probability`、`__builtin_unreachable`、`__builtin_trap`、`__builtin_abort`;
  - 内存: `__builtin_memcpy/memmove/memset/memcmp`、`__builtin_addressof`、`__builtin_launder`、`__builtin_object_size`;
  - 常量: `__builtin_constant_p`、`__builtin_is_constant_evaluated`（G28）;
  - 数学/位: `__builtin_clz/ctz/popcount/parity/ffs`、`__builtin_mul_overflow`、`__builtin_add_overflow`、`__builtin_bswap*`、浮点分类（`__builtin_isnan` 等）;
  - 函数特性: `__builtin_return_address`（debug 用）。
- 依据: basic_string.h 9、random.h 7、hashtable.h 7、stl_algo.h 6。

**G30. `__atomic_*`/`__sync_*` 内建确认与透传**
- 现状: C 路径完整（lock xadd/cmpxchg/mfence/LDAR/STLR/AMO），C++ 路径无专门处理、依赖透传。
- 方案: 验证 C++ 路径对 `__atomic_load_n`/`__atomic_store_n`/`__atomic_compare_exchange`/`__atomic_fetch_*`/`__atomic_thread_fence`/`__atomic_signal_fence` 及 `__sync_*` 旧式内建的原样透传；缺失则补表。atomic_base.h 286 处、futex.cc 依赖此能力。
- 依据: bits/atomic_base.h、src/c++11/futex.cc。

**G31. `__int128` 全链路确认**
- 现状: 词法 token 有（lexer.rs L58-59 KwInt128/KwUInt128），C 后端 rax:rdx 支持；常量求值（cpp_const_eval）与 sema 的类型运算需确认覆盖 128 位。
- 方案: 验证并补齐 `__int128` 在 sema 类型宽度、常量求值（i128 运算）、codegen 调用（`__divti3` 等 libgcc 辅助）三处。
- 依据: iterator_concepts.h 9、ranges_base.h 7、cpp_type_traits.h 4。

**G32. 向量扩展（GCC vector_size）**
- 现状: lower.rs 有 vec stubs（L6192-6200）与成员调用转换（L6400+），属性本身被剥离。
- 方案: 保留 `__attribute__((vector_size(n)))`，sema 生成向量类型，codegen 逐元素标量运算（C 路径已有）。libstdc++ 用量少，优先级低。
- 依据: bits/valarray 相关、`__GCC_VECTOR` 宏。

### 2.6 运行时 / 系统依赖（L1/L2）

**G33. `__cxa_*` 与真 Itanium ABI（建议编译 libsupc++ 而非自实现）**
- 现状: dcc 自实现简化版——`__dcc_typeinfo_t` 仅 `{ const char* name; }`、`__dynamic_cast` 无 vbase 二次偏移与类型校验、EH 为 SJLJ（gcc_compat.rs）。
- 方案: **放弃自实现，直接编译 libstdc++-v3/libsupc++（约 90 文件）**，获得与 g++ 二进制兼容的:
  - `__cxa_throw/rethrow/begin_catch/end_catch`（eh_throw.cc 等，DWARF personality）;
  - `__cxa_guard_acquire/release/abort`（guard.cc，**双字线程安全 guard**——dcc 当前 gcc_compat 是单线程状态机，不满足）;
  - `__cxa_allocate_exception`、`__dynamic_cast`（dyncast.cc 真实实现）;
  - typeinfo（tinfo*.cc）、`__cxa_vec_*`（vec.cc）、`__cxa_atexit`、`__cxa_pure_virtual`。
  前置: L0 语法能力 + G29 内建表 + DWARF EH 后端（见 G34）。
- 依据: libsupc++/ 目录文件清单、runtime.rs 现有 `CxxRuntimeSymbol` 表。

**G34. DWARF 异常处理（替换 SJLJ）**
- 现状: SJLJ（setjmp/longjmp）风格，与 Itanium ABI 不兼容。
- 方案: 引入 DWARF unwinding——`.eh_frame` 生成 + `__gxx_personality_v0`（由 libsupc++ 提供）+ landing pad 调度。dcc C 后端需先具备 `.eh_frame` 支持（若 dcc 面向自研后端则需新增 unwinding 表）。这是 L1 编译 libstdc++ 运行时异常路径的必要条件，也是与 g++ 混链 ABI 兼容的前提。
- 依据: libsupc++/eh_personality.cc、eh_throw.cc。

**G35. 线程抽象 `__gthread_*` 与 TLS `__thread`**
- 现状: 需确认 dcc C 后端 pthread 调用与 TLS 支持。
- 方案:
  - `__gthread_*`（gthr-posix.h 宏展开为 pthread 调用）→ 验证链接时 pthread 可用;
  - `__thread` TLS → C 后端需支持 TLS 变量（native ELF TLS 或 emutls）。libstdc++ 的 `src/c++11/mutex.cc` 用 `__thread void* __once_callable`。
- 依据: src/c++11/mutex.cc L34-35、src/c++11/thread.cc、locale_init.cc。

**G36. 直接 syscall**
- 现状: C 路径可编译 syscall 调用（内联汇编/函数调用）。
- 方案: futex.cc 直接 `syscall(SYS_futex, ...)`、`syscall(SYS_clock_gettime, ...)`——需要 `<sys/syscall.h>` 与 libc 链接，属正常 C 程序能力，验证即可。`[[unlikely]]` 属性需解析忽略。
- 依据: src/c++11/futex.cc L122/141/175/198/217/253/270、chrono.cc L57/85。

**G37. C++17 对齐分配**
- 现状: new/delete 仅 legacy 支持，native 不支持（NewDelete=false）。
- 方案: `operator new(size_t, align_val_t)`/`operator delete(void*, align_val_t)`（libstdc++ 用 `_GLIBCXX_ALIGNED_ALLOC`/`__builtin_align_up` 实现）;native 路径需支持 NewDelete 特性 + placement new 对齐。
- 依据: libsupc++/new_opa.cc、new_op.cc。

### 2.7 ABI / 链接层

**G38. 模板实例化符号（weak / COMDAT）**
- 现状: native 路径实例化符号的符号类型需确认。
- 方案: 隐式实例化函数/静态数据成员输出为 weak 符号（GCC `_ZTV`/模板函数默认 weak），汇编器/链接器需支持 COMDAT 合并。否则多 TU 重复定义。
- 依据: `extern template`（G10）与隐式实例化语义配套。

**G39. 符号版本脚本**
- 现状: 需确认 dcc 链接器是否支持 `--version-script`。
- 方案: 链接 libstdc++.so 时使用 `config/abi/pre/gnu.ver`，含 `extern "C++"` 块与 demangled 通配符（`std::[ABD-Z]*`）。若 dcc 链接器不支持，可降级为静态链接 libstdc++ 或使用系统 ld。
- 依据: config/abi/pre/gnu.ver L20 起 `GLIBCXX_3.4 {}` 节点。

**G40. 与系统 libstdc++ 共存**
- 现状: runtime.rs 注释"native 路径优先链接系统 libstdc++ 符号"。
- 方案: dcc 编译的 libstdc++ 若与系统 libstdc++ 混链会符号冲突。策略:
  1. 静态链接（`-static-libstdc++`）隔离;
  2. 或启用 `--with-versioned-namespace`（`__8` 命名空间宏）错开符号;
  3. 或在测试期始终静态链接 dcc 版 libstdc++。
- 依据: config/abi/pre/gnu-versioned-namespace.ver。

**G41. lambda 与局部类 mangling**
- 现状: lambda 降级为 `__lambda_N` 匿名 struct，无 `_ZZ...` 闭包编码；局部类 mangling 无。
- 方案: mangle.rs 补 Itanium ABI 闭包类型编码（`_ZZ<enclosing>EN<idx>E`）与局部类编码（`Z<fn>E` 前缀）。依赖 lambda 语义化（见 G42）。
- 依据: gcc/cp/mangle.cc、mangle.rs 现状（Auto/DecltypeAuto 输出占位符）。

**G42. lambda 语义化**
- 现状: parser 支持捕获/参数/返回，lower 降级为匿名 struct；无泛型 lambda、无闭包类型系统。
- 方案: sema 建立闭包类型（捕获字段布局按 Itanium ABI）、重载 `operator()`、转换到函数指针（无捕获时）；泛型 lambda（`auto` 参数 → 模板化 operator()）。libstdc++ 头大量使用 lambda（含泛型）。
- 依据: stl_algo.h、bits/ranges_algo.h 等。

**G43. 成员函数指针（PMF）完备化**
- 现状: MemberPtr 语法有、abi_layout 有 PMF 偏移宏。
- 方案: 按 Itanium ABI 实现 16 字节 PMF 表示（`{ptr, adj}` 及虚调用重定位），支持 `std::mem_fn`/`std::function` 所需的转换（PMF ↔ 函数指针）、`.*`/`->*` 调用。
- 依据: abi_layout.rs 现有 PMF 处理、bits/std_function.h。

**G44. RTTI 完整语义**
- 现状: 简化版（name-only typeinfo、简化 dynamic_cast）。
- 方案: 采用 G33（编译 libsupc++）后，`typeid`/`dynamic_cast` 语义对接真实现：vtable 的 typeinfo 槽位（OffsetToTop/TypeInfo）、`__dynamic_cast` 完整算法（src 类型 + 目标类型 + vbase 偏移二次修正 + 类型比较）。native 路径已有 `HirExprKind::TypeInfoRef/DynamicCast`，主要工作是 ABI 对齐。
- 依据: hir.rs TypeInfoRef/DynamicCast 节点、libsupc++/dyncast.cc。

### 2.8 不需要补的特性（明确排除）

- **内联汇编**: libstdc++ 与 gcc/cp 均为 0 处（gcc/cp 的 `__asm__` 仅出现在声明尾缀重命名，parser 已跳过）。
- **可变参数宏**: gcc/cp 为 0 处。
- **协程**: libstdc++ 头不依赖（`std::generator` 是 C++23 标准库协程，但 dcc 自带 coroutine.rs 已可处理 legacy 路径）。
- **模块**: libstdc++ 头不使用 modules。
- **虚继承**: 仅 ostream.h 1 处，native 已支持 VirtualInheritance。

## 3. 实施路线图

按依赖顺序分 8 个 Phase。每个 Phase 有独立的验收标准（DoD）。

### Phase 0 — 头文件可解析（L0 语法门槛）
覆盖缺口: G1-G4（词法）、G5/G7/G8/G9/G10（语法）、G19 属性保留（白名单 + `#pragma` 吞掉）。
DoD: `dcc -x c++ -fsyntax-only` 能解析 `include/std/type_traits`、`bits/char_traits.h`、`bits/move.h` 前 500 行而不报 Unsupported（允许 static_assert 求值暂缺报错但可跳过）。

### Phase 1 — 模板核心（L0 语义门槛）
覆盖缺口: G12 包展开、G13 全特化、G14 路线 A（文本替换 + sema 名字查找）、G16 SFINAE、G17 变量模板、G11 模板模板参数、G6 折叠表达式。
DoD: 可实例化 `std::tuple<int, double, std::string>`（含 `make_index_sequence` 模式）与 `std::common_type_t<int, double>`。

### Phase 2 — 语义完备（L0 完成）
覆盖缺口: G20 重载决议、G21 ADL、G22 访问控制、G18 引用折叠、G24 `__is_*` trait 内建、G15 两阶段查找。
DoD: `std::vector<int>`/`std::string`/`std::unique_ptr<T>` 全头文件可编译，`#include <algorithm>` 的 sort/find 可实例化。

### Phase 3 — 常量求值（L0 收尾）
覆盖缺口: G25 constexpr 解释器、G26 if constexpr 语义、G27 consteval、G28 `__builtin_is_constant_evaluated`。
DoD: `std::array` 的 constexpr 路径、`std::is_constant_evaluated()` 分派、`static_assert` 全量生效（libstdc++ 148 处）。

### Phase 4 — 内建与扩展（L0/L1 共用）
覆盖缺口: G29 `__builtin_*` 表、G30 原子透传确认、G31 `__int128` 全链路、G32 向量。
DoD: libstdc++ 头中 68 处 `__builtin_*` 全部可识别且语义正确（`__builtin_constant_p`/`__builtin_addressof`/`__builtin_launder` 必须）。

### Phase 5 — L0 门禁
覆盖缺口: 80 标准头 + 160 bits 头全量编译（先 `-fsyntax-only`，后链接最小可执行）。
DoD: 用 dcc 编译一个只含 `#include <vector> <string> <map> <memory> <algorithm>` 的 hello 程序并运行通过（对照 g++ 行为）。

### Phase 6 — libstdc++ 运行时（L1）
覆盖缺口: G33-G37（libsupc++ 编译、DWARF EH、__gthread/TLS、syscall、对齐 new/delete）、G38-G41、G43-G44。
DoD: 编译 `src/c++98/*.cc` 与 `src/c++11/*.cc` 全部通过；`libstdc++.a` 可静态链接；dcc 编译的 TU 与 g++ 编译的 TU 混合链接运行一致。

### Phase 7 — 标准库测试套件（L1 验收）
覆盖缺口: 无新特性，跑 `libstdc++-v3/testsuite` 的 compile/link/run 子集（23_containers、20_util、17_intro）。
DoD: testsuite 中 vector/string/unique_ptr/optional/variant 的 run 用例通过率 ≥ 90%。

### Phase 8 — gcc/cp（L3，远期，独立立项）
覆盖缺口: 无新增语言特性（语法门槛已由 L0-L2 覆盖），主要是工程问题——
1. gengtype 预生成 `gt-cp-*.h`（20 个 .cc 依赖）;
2. `gcc/` 顶层基础设施头（tree.h、vec.h、diagnostic.h 等 C++ 头）——本质是另一个巨大 C++ 代码库;
3. 870 万行规模的编译时间与内存（模板实例化深度、宏展开）。
DoD: `cc1plus` 可编译简单 C++ 程序并输出 RTL（可对照 [gcc_build_guide.md](./gcc_build_guide.md) 的 M26.6 门禁）。

## 4. 验证策略

1. **编译期对照**: 对同一 TU，dcc 与 g++ 分别 `-S`，语义一致（ABI 层用 nm 对照符号）。
2. **混链测试**: dcc 编译的 `main.cc` 链接 g++ 编译的 libstdc++.a，运行结果一致——直接验证 ABI 兼容（vtable/typeinfo/EH 布局）。
3. **testsuite 门禁**: 复用 libstdc++-v3/testsuite 的 compile-only 用例（零运行依赖）作为回归集。
4. **dcc 自带 stdlib 迁移**: `src/cpp/stdlib/headers/` 的测试逐步切换到真 libstdc++ 头，保持现有 828 集成测试通过。
5. **性能基线**: 记录模板实例化时间/峰值内存（`-ftime-report` 等价物），防止路线 B 迁移导致回归。

## 5. 风险与缓解

| 风险 | 影响 | 缓解 |
|---|---|---|
| 模板文本替换 → 语义实例化迁移破坏现有用例 | 高 | 路线 A 渐进式；native/legacy 双路径并行，Auto 模式兜底回退 |
| `[[no_unique_address]]` 等属性剥离导致 ABI 布局错误 | 高 | Phase 0 优先语义化该属性并加入 abi_layout 对照测试 |
| SJLJ EH 与 g++ 混链不兼容 | 高 | 以编译 libsupc++（DWARF EH）为 L1 前置，放弃自实现 EH |
| `__is_*` trait 内建量大（数十个）且依赖重载决议 | 中高 | 按使用频率排序（is_convertible/is_constructible 依赖 G20 先做） |
| 编译时间: 模板实例化 + 8 位行头文件 | 中 | 模块化编译、实例化缓存复用（template.rs 已有 inst_cache）、预编译头 |
| gcc/cp 前置工具链（gengtype） | 中（L3） | 先做 libstdc++（L0-L2），gcc/cp 独立立项 |
| 符号冲突（dcc libstdc++ vs 系统 libstdc++） | 中 | 静态链接或 versioned namespace |

## 6. Definition of Done

1. dcc 用 native 路径解析全部 libstdc++ 头文件（80 std + 160 bits）无 Unsupported 报错（Phase 5 门禁）。
2. `static_assert`/`if constexpr`/concept 约束在实例化中正确生效（不依赖 legacy 降级）。
3. `__builtin_*`/`__is_*`/`__atomic_*`/`__int128` 在 C++ 路径全部语义化。
4. libstdc++ 运行时（src/*.cc + libsupc++）由 dcc 编译链接为静态库，与 g++ 产物 ABI 兼容（混链测试通过）。
5. libstdc++-v3/testsuite 核心容器/工具用例通过率 ≥ 90%。
6. gcc/cp 作为独立里程碑，达到 [gcc_build_guide.md](./gcc_build_guide.md) 的 cc1plus 门禁后视为完成。
7. 全程无回归: 现有 1498 lib 单测 + 828 集成测试保持全绿。

## 附录 A. 特性缺口 → Phase 映射速查

| 缺口 | 模块 | Phase |
|---|---|---|
| G1-G4 词法（raw string/前缀/UDL/分隔符） | lexer.rs | 0 |
| G5/G7/G8/G9/G10 语法（结构化绑定/尾置返回/static_assert/inline ns/extern template） | parser.rs | 0 |
| G19 属性白名单 + pragma | parser/lower | 0 |
| G12/G13/G14/G16/G17/G11/G6 模板 | template.rs/sema | 1 |
| G20/G21/G22/G18/G24/G15 语义 | sema.rs | 2 |
| G25-G28 常量求值 | cpp_const_eval.rs | 3 |
| G29-G32 内建/扩展 | sema/lower_ir | 4 |
| L0 门禁 | — | 5 |
| G33-G37/G38-G41/G43-G44 ABI/运行时 | gcc_compat/abi_layout/mangle | 6 |
| testsuite | — | 7 |
| gcc/cp | 工程 | 8 |

## 附录 B. 关键证据文件索引

- dcc 现状: `src/cpp/feature_matrix.rs`、`src/cpp/template.rs`、`src/cpp/sema.rs`、`src/cpp/cpp_const_eval.rs`、`src/cpp/mangle.rs`、`src/cpp/lower.rs`（L4761/L4851/L6532/L7730）、`src/cpp/gcc_compat.rs`
- libstdc++ 特征: `/root/gcc-15.3.0/libstdc++-v3/include/bits/move.h`、`include/std/concepts`、`include/std/type_traits`、`include/bits/stl_vector.h`、`include/bits/unique_ptr.h`、`include/bits/c++config`、`libsupc++/exception.h`
- 运行时依赖: `libstdc++-v3/src/c++11/futex.cc`、`mutex.cc`、`thread.cc`、`chrono.cc`
- 配置依赖: `libstdc++-v3/configure.ac`、`acinclude.m4`、`include/Makefile.am`
- 符号版本: `libstdc++-v3/config/abi/pre/gnu.ver`
- 既有尝试: `docs/gcc_build_guide.md`、`docs/gcc_build_progress.md`、`scripts/cc1plus_bootstrap.sh`
