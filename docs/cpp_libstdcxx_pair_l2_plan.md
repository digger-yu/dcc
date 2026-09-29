# C++ 前端 L2 完整化: libstdc++ std::pair + inline namespace + concepts 实施方案

- 状态: 已批准, 实施中
- 前置: `cxx-native-foundation-fix-plan.md` 已完成并全绿; B4 不完整类模型 / `_Float32` 扩展浮点 / 全局函数指针声明 / namespace 属性容错 已上线; `--component=libcpp` 当前统一首错 `unsupported opaque declaration:  std {`, 即真实 `stl_pair.h` 里 pair 模板体
- 目标: 把 dcc C++ native 前端推进到**同时覆盖 C++17 与 C++20** 语义层, 使 14 个 libcpp 真 TU 全部 native 通过并 `ar` 成 `libcpp.a` (dcc 产物, 非宿主 cc)
- 约束:
  - 零回归: 每一步都必须保持既有全量测试通过 (lib/integration/cpp_abi/errors)
  - 全局彻底, 不留分阶段缺口: 每个能力项都必须端到端落地并测试; "注释里写以后补" 一律禁止
  - Design-First (承接 `cpp_native_gcc_convergence_plan.md` §2): 语义模型先于实现, 不因为"编不过"降级成 skip
  - 资源约束: 所有构建/测试/探测一律 `-j2` / `--test-threads=2` / `JOBS=2` + `nice -n 10`, 不并发多大型构建
  - 脚本一律 bash, 不用 python
- 交付物: 本文档; §2 每项能力的实现 + 夹具 + 交叉验证 + feature_matrix 翻绿; 端到端 DoD 见 §6

## 0. 实测事实基线

- `inline namespace __cxx11 __attribute__((__abi_tag__("cxx11")))` 位于 `c++config.h:341/345/417`, 由 `_GLIBCXX_BEGIN_NAMESPACE_CXX11`/`_GLIBCXX_NAMESPACE_CXX11` 宏在 `<string>`/`<iostream>` 等链路上使用; `_GLIBCXX_INLINE_VERSION 0` → `_GLIBCXX_BEGIN_NAMESPACE_VERSION` 与 `_GLIBCXX_END_NAMESPACE_VERSION` 展开为空, 不是 pair 阻塞源
- `stl_pair.h` 关键形态 (行 78/165/196/242/460/815 是 `#if __cpp_lib_concepts`/`__cplusplus` 分支):
  - `struct pair : public __pair_base<_T1,_T2>` — 依赖基类
  - `typedef _T1 first_type; typedef _T2 second_type;` — 依赖 typedef
  - `constexpr pair(const pair&) = default;` / `pair& operator=(pair&&) = default;` — `= default` / `= delete`
  - `template<typename... _Args1, size_t... _Indexes1, typename... _Args2, size_t... _Indexes2> pair(tuple<_Args1...>&, tuple<_Args2...>&, _Pct, index_sequence<_Indexes1...>, index_sequence<_Indexes2...>)` — 变参 + 非类型 pack + 类型 pack 展开
  - `noexcept(__and_<__is_nothrow_swappable<_T1>, __is_nothrow_swappable<_T2>>::value)` — 依赖 noexcept 表达式
  - `swap(const pair&) const noexcept(...) requires is_swappable_v<const _T1> && is_swappable_v<const _T2>` — C++20 `requires`
  - `constexpr explicit(__not_<__and_<__is_implicitly_default_constructible<_T1>, ...>>()) pair() requires is_default_constructible_v<_T1> && is_default_constructible_v<_T2>` — C++20 `explicit(bool)` + `requires`
  - `if constexpr (is_constructible_v<_T1,_U1>) return is_constructible_v<_T2,_U2>;` — `if constexpr` 折叠
  - `#if __cpp_lib_three_way_comparison && __cpp_lib_concepts` 分支的 `operator<=>` — C++20 三向比较

## 1. 语义模型总纲 (Design-First, 无缺口)

所有能力都建立在以下四条**全局不变量**上, 违反任一即视为分阶段缺口:

1. **模板生命周期三段** ([temp.res]/8, [temp.inst], [temp.spec]): 声明登记 (declare) → 依赖名两阶段查找 (two-phase lookup) → ODR-use 时实例化成员体 (deferred body sema)。未实例化的模板体里的依赖名/未定义 traits 引用**不得**在声明期报硬错, 也**不得**简单丢弃, 必须保留 AST 结构供实例化期完整 sema
2. **SFINAE 语义** ([temp.deduct]/8): 在替换失败时**从候选集剔除**, 不是编译错误; 仅当全部候选被剔除才报"no matching"。这是重载决议的一部分, 不是错误恢复
3. **约束求值** ([concepts], [temp.constr], [expr.prim.req]): `requires` 表达式在实例化点折叠为 bool; 约束不满足 = 候选剔除; 全部不满足才硬错。concept 定义等价于一个折叠到 bool 的约束函数
4. **Inline namespace** ([namespace.inline]/3): 内联命名空间的成员在外层命名空间作用域**直接可见**; abi_tag 属性参与 Itanium B.6 mangle 编码

## 2. 完整能力清单 (12 项, 每项端到端落地)

每一项给出: 阻塞的 pair 具体形态 / 代码改动点 / 交叉验证夹具 / 完成判据。

### A. 变参模板与 pack 展开

- 阻塞: pair 的 piecewise 构造签名 `template<typename... _Args1, size_t... _Indexes1, typename... _Args2, size_t... _Indexes2> pair(tuple<_Args1...>&, tuple<_Args2...>&, _Pct, index_sequence<_Indexes1...>, index_sequence<_Indexes2...>)`
- 改动点:
  - `src/cpp/parser.rs`: 类型参数 pack (`typename... Name`) 与非类型 pack (`size_t... Name`) 语法; 类型位置的 pack 展开 `Foo<Ts...>`; `sizeof...(Name)`; 折叠表达式 `(Ts op ...)` / `(... op Ts)`
  - `src/cpp/template.rs`: pack 绑定与替换; pack 长度折叠; 索引 pack 生成 (`make_index_sequence<N>` 折到 `index_sequence<0,1,...,N-1>`)
  - `src/cpp/sema.rs`: 依赖 pack 的名字查找路径
- 交叉验证夹具: `tests/fixtures/cpp_abi_template/variadic_pair_ctor.cpp` (pack 展开 + 索引 pack + 折叠表达式)
- 完成判据: `--component=libcpp` 首错不再出现在 pair 的 piecewise 签名; 夹具与 g++ stdout 逐行一致

### B. 类模板的依赖基类与继承链

- 阻塞: `struct pair : public __pair_base<_T1,_T2>`
- 改动点:
  - `src/cpp/sema.rs::parse_base_specs` (当前要求基类必须 `is_class_complete`, 但类模板依赖基类在**声明期**必然不完整) — 引入 `is_class_template_scope` 判定, 类模板声明期允许"未实例化的依赖基类"作为占位, 到实例化点再强制完整
  - `src/cpp/template.rs`: 实例化派生类时同步触发基类实例化; 基类闭包遍历
  - `src/cpp/abi_layout.rs`: 依赖基类子对象布局
- 交叉验证夹具: `tests/fixtures/cpp_abi_template/dependent_base_lookup.cpp` (`pair<int,string>` 继承 `__pair_base<int,string>`, 成员查找走基类链)
- 完成判据: B4 已建立的"完整类型才允许按值/基类"闸门与模板依赖基类共存无回归

### C. 成员模板 (类模板里的模板成员)

- 阻塞: `template<typename _U1, typename _U2> static constexpr bool _S_constructible() noexcept`; `template<typename _U1, typename _U2> constexpr pair(_U1&&, _U2&&)`
- 改动点:
  - `src/cpp/parser.rs`: 类体内 `template<...>` 前缀识别, 目前 `build_class_member` 的 `CppDecl::Template => Ok(())` 静默丢弃 → 改为登记 pending (结构保留, 语义延迟)
  - `src/cpp/template.rs`: 外层类模板参数与内层成员模板参数**分层作用域**, 内层在类模板实例化后仍保持未绑定直到函数实例化
  - `src/cpp/sema.rs`: pending 成员模板在类实例化时把外层参数 substituted 进去; 内层参数留到函数实例化点再绑
- 交叉验证: 夹具 `tests/fixtures/cpp_abi_template/member_template_pair_sconstructible.cpp` 触发 `_S_constructible<int,int>()` 并 printf 结果
- 完成判据: pair 里所有 `_S_*` static constexpr 函数可解析, 实例化后可调用

### D. `= default` / `= delete` 特殊成员函数

- 阻塞: `constexpr pair(const pair&) = default;` / `pair& operator=(pair&&) = default;`; 多处 `= delete`
- 改动点:
  - `src/cpp/parser.rs`: 声明尾 `= default` / `= delete` token 序列识别, 落到 `FnSym` 标记 (当前可能作为 init 表达式解析)
  - `src/cpp/sema.rs`: `FnSym` 新增 `is_defaulted` / `is_deleted`; defaulted 特殊成员触发**合成语义** (类似现有 `synthesize_implicit_default_ctor`, 扩展到 copy/move/copy-assign/move-assign/dtor); deleted 保留符号, 调用它必须硬错
  - `src/cpp/lower_ir.rs`: deleted 调用点报 `use of deleted function`
- 交叉验证: 夹具 `tests/fixtures/cpp_abi_lifecycle/defaulted_deleted_specials.cpp`, 覆盖 `= default` 与 `= delete` 的正负向
- 完成判据: pair 声明里的 `= default` 特殊成员不报错; 对 `= delete` 的调用点硬错

### E. `constexpr` 函数体折叠与 `if constexpr`

- 阻塞: `constexpr bool _S_constructible() { if constexpr (is_constructible_v<_T1,_U1>) return is_constructible_v<_T2,_U2>; else return false; }`; `constexpr` 构造/成员
- 改动点:
  - `src/cpp/cpp_const_eval.rs`: 从"折叠字面量+简单 binop"扩展到**函数体折叠** (constexpr 函数在常量上下文中被调用时递归折叠), 支持 `if constexpr` 分支选择、成员访问、构造
  - `src/cpp/sema.rs`: 声明为 `constexpr` 的 static 数据成员在常量环境中求值
- 交叉验证: 夹具 `tests/fixtures/cpp_abi_template/constexpr_pair_helpers.cpp` 用 `_S_constructible` 驱动 `static_assert`
- 完成判据: `static_assert(pair<int,int>::_S_constructible<int,int>())` 与 g++ 折出同值

### F. 依赖 `noexcept` 表达式

- 阻塞: `swap(pair&) noexcept(__and_<__is_nothrow_swappable<_T1>, __is_nothrow_swappable<_T2>>::value)`; `pair(...)` 构造函数同类形态
- 改动点:
  - `src/cpp/parser.rs`: noexcept 说明符允许携带**未折叠的依赖表达式** (当前只接常量), 存 `NoexceptSpec::Dependent(CppExpr)`
  - `src/cpp/template.rs`: 实例化时 substituted 参数后再折叠
  - `src/cpp/sema.rs::mangle_ctor_can_throw` / `mangle_dtor_can_throw`: 依赖 noexcept 不可折时保守视为 can-throw (语义安全, 不影响正确性)
- 交叉验证: 夹具 `tests/fixtures/cpp_abi_template/dependent_noexcept_pair_swap.cpp`
- 完成判据: pair 的 `swap` / 构造函数声明里依赖 noexcept 不再报 unsupported; 实例化后 `noexcept(...)` 折出与 g++ 一致的 bool

### G. SFINAE 与 `enable_if`

- 阻塞: pair 的 C++17 分支大量用 `typename = __enable_if_t<...>` 约束构造重载集; `_S_constructible` 通过 `if constexpr` 表达等价约束
- 改动点:
  - `src/cpp/type_traits.rs` (新): 覆盖 `is_default_constructible_v` / `is_implicitly_default_constructible` / `is_constructible_v` / `is_nothrow_swappable` / `__and_`/`__or_`/`__not_` / `void_t` 常量折叠, 与 libstdc++ `<type_traits>` stub 一一对应
  - `src/cpp/template.rs`: `enable_if<cond, T>::type` 在 cond=false 时替换失败 → **剔除候选**; 在候选构建阶段用 SFINAE-aware 的 substitution (不是硬错)
  - `src/cpp/sema.rs::resolve_overload_set`: 在重载集上跑 SFINAE 剔除
- 交叉验证: 夹具 `tests/fixtures/cpp_abi_template/sfinae_pair_ctors.cpp` (多个 pair 构造候选, 只有一个满足 enable_if)
- 完成判据: pair 的多构造重载在 SFINAE 下正确选定; 全部候选失败时报"no matching constructor", 不报替换期硬错

### H. C++20 concepts 与 `requires` 子句

- 阻塞: `swap(const pair&) const noexcept(...) requires is_swappable_v<const _T1> && is_swappable_v<const _T2>`; `constexpr explicit(...) pair() requires is_default_constructible_v<_T1> && is_default_constructible_v<_T2>`; `<concepts>`/`<ranges>` 里 `concept` 定义
- 改动点:
  - `src/cpp/parser.rs`: `parse_requires_clause` (在 declarator 与函数体之间); `concept <name> = <constraint>;` 声明; `typename`/`class` 后接 concept-name (`std::equality_comparable auto`)
  - `src/cpp/concepts.rs` (新): concept 声明登记、约束表达式求值、原子约束/规范化 (把 `A && B` 拆到合取范式)、subsumption (约束包含判定, [temp.constr.order])
  - `src/cpp/template.rs`: 在实例化前用 substituted 参数折叠 requires, 不满足即候选剔除 (与 G 同一管道)
  - `src/cpp/sema.rs`: requires 参与重载决议; 只有当所有候选都失败才报约束不满足
- 交叉验证: `tests/fixtures/cpp_abi_template/{concepts_requires_pair_cxx20, constraint_subsumption}.cpp`
- 完成判据: C++20 分支的 `#if __cpp_lib_concepts` 打开后, pair 里所有 requires-clause 折叠正确, 不满足 = 候选剔除, 全失败 = 硬错

### I. `explicit(bool)` 条件 explicit

- 阻塞: `explicit(__not_<...>())  pair()` 等 C++20 pair 构造形态
- 改动点:
  - `src/cpp/parser.rs`: `explicit` 关键字后接 `(...)` 的解析 (当前只有裸 `explicit`)
  - `src/cpp/sema.rs`: 折叠到 bool 后决定 ctor 是否参与隐式转换 (重载决议入口用)
- 交叉验证: `tests/fixtures/cpp_abi_template/explicit_bool_ctor.cpp` (同一个 ctor 在不同参数下 implicit/explicit 都覆盖)
- 完成判据: `explicit(cond)` 折叠与 g++ 一致; pair 隐式转换测试正确

### J. `<tuple>` 与 `piecewise_construct_t` 依赖链

- 阻塞: pair 的 piecewise ctor 签名引用 `std::tuple<_Args...>` / `std::index_sequence<_Is...>` / `std::piecewise_construct_t`
- 改动点:
  - `src/cpp/stdlib/headers/tuple` 扩展: `template<typename... _Elements> class tuple` 声明与 `__detail::_Inherit<_Idx, _T>` 布局
  - `src/cpp/stdlib/headers/utility` 或 `<bits/utility.h>` stub: `piecewise_construct_t` / `piecewise_construct` 全局常量 / `in_place_t` 族
  - `src/cpp/stdlib/headers/{type_traits,concepts}` 与 §G/§H 的 traits/concepts 定义同步
  - `src/cpp/template.rs`: `make_index_sequence<N>` 折叠到 `index_sequence<0,...,N-1>` (递归模板实例化, 需要 §A pack + §C 成员模板支撑)
- 交叉验证: `tests/fixtures/cpp_abi_template/tuple_index_sequence.cpp` (piecewise 构造 pair<pair<int,int>, pair<string,string>>)
- 完成判据: pair piecewise 构造能实例化, 与 g++ 结果一致

### K. 三向比较 `<=>` 与 `__cpp_lib_three_way_comparison`

- 阻塞: `stl_pair.h:815+` 的 `#if __cpp_lib_three_way_comparison && __cpp_lib_concepts` 分支的 `operator<=>`; `<=>` 与 `strong_ordering`/`weak_ordering`/`partial_ordering` 类模板
- 改动点:
  - `src/cpp/parser.rs`: 新增 `<=>` token 与 `operator<=>` 函数声明解析
  - `src/cpp/lexer.rs` (C++ 侧): 加入 `<=>` 三字符 token
  - `src/cpp/hir.rs`: `CppExpr::Spaceship` 变体 (至少保留结构; 求值可依赖后端合成)
  - `src/cpp/sema.rs`: `std::strong_ordering`/`weak_ordering`/`partial_ordering` 类模板声明; `compare_three_way` concept
  - `src/preproc.rs`: 确保 `__cpp_lib_three_way_comparison` 依 `--std=c++20` 正确定义/未定义 (从 c++config.h 读到即可, 不需要 dcc 自己硬编)
  - `src/cpp/lower_ir.rs`: `<=>` 表达式可先只保留为未求值的依赖调用 (但**必须能声明**, libcpp 走 c++17 时不启用该分支)
- 交叉验证: `tests/fixtures/cpp_abi_template/three_way_pair.cpp` (pair 的 `<=>` 与 g++ 一致)
- 完成判据: C++20 分支打开后 pair 声明不再报 unsupported

### L. 两阶段查找与惰性成员体 (贯穿性, 其他能力的时序前提)

- 阻塞: 以上 A–K 都要求"类模板声明期不 sema 成员体, 到实例化点再做完整 sema"这条统一不变量; 目前 `src/cpp/sema.rs::build_class_member` 对 `CppDecl::Template` 静默丢弃 (见 §C), 对 `Class => Ok(())` 里的成员体走 eager sema, pair 里的依赖名会被当作未声明 → 硬错
- 改动点:
  - `src/cpp/sema.rs`: 类模板声明期, 成员体保留 C++ 层 AST 到一个 `pending_bodies: HashMap<ClassTemplateId, Vec<...>>`; 成员签名仍登记, 但体内 `Dependent` 允许出现
  - `src/cpp/template.rs`: 实例化流程 = 参数替换 → 签名重 sema → 成员体重 sema → 注册到 HIR
  - `src/cpp/hir.rs`: 类模板的 AST 表示与实例化后 AST 的分离; `HirTopLevelDecl::ClassTemplate { decl: CppDecl::Class, instantiated: HashMap<args_key, ClassId> }`
- 交叉验证: `tests/fixtures/cpp_abi_template/lazy_member_body.cpp` (pair 声明期依赖名合法, 实例化后才要求解析)
- 完成判据: `#include <utility>` 单 TU 走完整管线不报硬错; pair 未被 ODR-use 时不生成任何 IR; 被 ODR-use 时成员体 sema 完整

#### Step 1 (L) 设计注记 (尝试与回归教训, 下一轮实施直接参考)

**已尝试并失败的方案**: 将 `build_function` / `build_constructor` 中 `if body.is_decl { return }
` 后新增一个 `if self.defer_bodies { 入队 + return }` 分支, 在 `build_decl::Class` 成员循环 +
隐式合成阶段把 `defer_bodies=true` 开起来, 成员循环结束后、pop 之前 drain。实测导致 5
个现有测试回归 (build_hir_constructor_and_destructor / dtor_body_entry_binds_current_class_vptr
/ delete_picks_the_dtor_call / lower_diamond_virtual_inheritance_native / multi_inheritance_*),
**已 revert**。

**失败根因 (必须在新方案中一并解决)**:

1. `build_class_member` 的 `CppDecl::Dtor` / `CppDecl::Ctor` 分支 **并非“只调 build_function
   建符号”** —— 它们在 build_function 返回后还会做三类后置动作, 依赖 `hir.functions.last()`:
   - virtual dtor 入口写自身 vptr: `self.hir.functions.last().unwrap().body` 后前插
     `SetVTablePtr` 语句 (sema.rs:8181 附近)。
   - `emit_dtor_base_variant` 发射 D2 (基子对象) 变体, 它自己会 push `hir.functions`。
   - Constructor 的 C2 变体同理 (build_constructor 内部会 push 两份 hir.functions)。
   将 defer 注在 build_function 内部, 上面三步会在未真正 push hir.functions 时错把上一个
   无关函数当成本次, 产生回归。

2. `synthesize_implicit_default_ctor` / `synthesize_implicit_dtor` 也会 `hir.functions.last().unwrap()`
   取回自己那个 body。它们如果在 `defer_bodies=true` 下调用, 会只入队不入 hir.functions,
   `.last()` 直接 panic。因此 synthesize **必须在 defer=false 下跑**。

**推荐的下一轮实施方向**:

- 延后的粒度上提到 `build_class_member`: 对 `MemberFn` / `Ctor` / `Dtor` 且带真实 body 的
  成员, 先只登记签名 (建符号、进 `class_infos.methods`/`constructors`/`destructor`、分配
  vtable slot 等不依赖 hir.functions 的部分) 并把“重跑完整 `build_class_member`”入队, 而不
  是仅重跑 `build_function`。drain 时 `defer_bodies=false`, 直接重入 `build_class_member`, 上面
  三类后置动作自然一次性就位。
- 签名阶段与 drain 阶段不能重复登记符号: 给 `build_function` / `build_constructor` 传
  `FnSym::existing(Some(sym))` 以复用签名阶段创建的符号。
- `build_decl::Class` 与嵌套类分支 (build_class_member 的 Class 分支) 都需 defer 开 → 成员
  循环 → defer 关 → drain → 隐式合成 → pop; drain 必须在 synthesize 之前。
- 回归卡点: 上述 5 个失败名先作为验收夹具, 确认新方案不重现回归。

## 3. 交叉验证与验收 (逐条与 g++ 一致)

- 每一能力 A–L 都提供:
  - 最小 g++ 交叉验证夹具 (新分组 `tests/fixtures/cpp_abi_template/`, 同步 `tests/cpp_abi_gcc_cross_check.rs` 的 `COVERED` 常量与分组测试)
  - 完整头级验证: `#include <utility>` 单独 TU 编过 + 实例化 `pair<int,std::string>` 运行结果与 g++ stdout 逐行一致 (含 abi_tag mangling 走 §I 交叉)
- 端到端 DoD: `scripts/gcc_tree_cpp_probe.sh --component=libcpp` 通过数 14/16 (`makeucnid`/`makeuname2c` 是构建期表生成器, 与 L0 主验收分离), 且 `libcpp.a` 由 dcc 产物 `ar` 组成, 用 `readelf` 证明 `.o` 全部出自 dcc (对齐 `cpp_native_gcc_convergence_plan.md` L0 判据)
- 语言级别: `-std=c++17` 与 `-std=c++20` 两条 pair 分支都要有正/负夹具; `requires` 不满足是**候选剔除**, 不是硬错; 全部不满足才硬错

## 4. 实施顺序 (依赖拓扑, 串行, 每步闭环)

按不变量的依赖强度严格排序; 一步跑绿 (回归全通过) 才进下一步; 单线程推进, 每步 `cargo build -j2` → `cargo test -j2 --lib -- --test-threads=2` → `--test cpp_abi_gcc_cross_check` → `--test integration` → `--test errors` → `JOBS=2 nice -n 10 bash scripts/gcc_tree_cpp_probe.sh --component=libcpp` 观察首错变化。

- **Step 1 (L)**: 两阶段查找 + 惰性成员体 (地基, 其他能力的 sema 时序前提)
- **Step 2 (A)**: 变参模板与 pack 展开
- **Step 3 (J)**: `<tuple>` / `index_sequence` / `piecewise_construct_t` 扩展 (pair piecewise 签名解析所需)
- **Step 4 (B)**: 类模板依赖基类
- **Step 5 (C)**: 成员模板 (外层类模板参数 + 内层成员模板参数分层)
- **Step 6 (D)**: `= default` / `= delete` 特殊成员函数合成
- **Step 7 (E)**: constexpr 函数体折叠 + `if constexpr`
- **Step 8 (F)**: 依赖 noexcept 表达式 (声明期挂起, 实例化期折叠)
- **Step 9 (G)**: SFINAE + `enable_if` 候选剔除 (新 `src/cpp/type_traits.rs`)
- **Step 10 (H)**: concepts + requires-clause (新 `src/cpp/concepts.rs`)
- **Step 11 (I)**: `explicit(bool)` 条件 explicit
- **Step 12 (abi_tag)**: inline namespace + abi_tag mangling — 必须与 `cpp_native_gcc_convergence_plan.md` 的 **D5 (自由函数 Itanium mangling)** 同批落地 (abi_tag 编码依赖真 Itanium 编码, `symbol_link_name` 唯一定义点)
- **Step 13 (K)**: `<=>` 声明与 `__cpp_lib_three_way_comparison` 分支启用
- **Step 14**: `#include <utility>` 端到端 g++ 交叉; libcpp 逐 TU 推进, 把每条新暴露缺陷按 §2 结构逐项登记并回补步骤

硬串行: Step 1 → 2 → 4 → 5 → 9 → 10。Step 12 可与 Step 3–9 并行准备, 但必须与 D5 合并落地。Step 13 需 Step 10 完成 (concepts 是 `<=>` 分支的门)。

## 5. 影响面 (文件级)

- Parser: `src/cpp/parser.rs` (变参/pack 展开/requires/explicit(bool)/`<=>`/inline namespace + abi_tag/`= default`+`= delete`/依赖 noexcept 语法/成员模板保留不丢)
- 词法: `src/cpp/lexer.rs` (`<=>` 三字符 token)
- AST/HIR: `src/cpp/hir.rs`, `src/cpp/parser.rs` 的 `CppDecl::Template` 结构保留 (不再丢)
- Sema: `src/cpp/sema.rs` (类模板声明期不 sema 体; 依赖基类占位; inline namespace 提升到外层作用域; `explicit(bool)` 影响隐式转换; requires 参与重载决议)
- Template engine: `src/cpp/template.rs` (instantiate 流程, 参数 pack 替换, 依赖名/表达式在 substituted 后再完整 sema; `make_index_sequence` 折叠)
- Concepts (新): `src/cpp/concepts.rs` (concept 声明、约束表达式求值、合取范式规范化、subsumption 判定)
- Const-eval: `src/cpp/cpp_const_eval.rs` (constexpr 函数体折叠、`if constexpr`、requires 表达式、依赖 noexcept)
- Type traits (新): `src/cpp/type_traits.rs` (`is_*` 系列与 `__and_/__or_/__not_`/`void_t`/`enable_if` 常量折叠)
- Mangling: `src/cpp/mangle.rs` (abi_tag 编码), `src/cpp/lower_ir.rs::symbol_link_name` (inline namespace + abi_tag 合并, 与 D5 同批)
- ABI layout: `src/cpp/abi_layout.rs` (依赖基类子对象, inline namespace 参与 vtable/typeinfo 名字)
- Stdlib stubs: `src/cpp/stdlib/headers/{utility,tuple,type_traits,concepts}` (扩展或改为直接解析真 libstdc++ 头, 依 Step 14 决策)
- Feature matrix: `src/cpp/feature_matrix.rs` 把 §2 十二项能力登记为 `CppFeature` 项, 逐条打开 `native_supported` + `end_to_end_supported`
- 驱动/预处理器: `src/preproc.rs` 依 `--std=c++NN` 定义 `__cpp_lib_concepts` / `__cpp_lib_three_way_comparison` / `__cpp_deduction_guides` / `__cpp_nontype_template_parameter_auto` 等检测宏, 与真 libstdc++ `<version>` 头一致
- 测试: 新分组 `tests/fixtures/cpp_abi_template/` (~12 个夹具, 对应 §2 每项一个) + `tests/fixtures/cpp_abi_mangling/` (inline namespace + abi_tag, 与 D5 合并); 新分组必须同时加 `cpp_abi_groups_are_all_covered` 的 `COVERED` 条目与分组测试函数

## 6. DoD (缺一不可)

1. std::pair 完整解析 (C++17 + C++20 两条分支)、声明期不硬错、ODR-use 时实例化正确、成员体 sema 与代码生成在实例化点完整
2. `#include <utility>` 单 TU 在 `-std=c++17` 与 `-std=c++20` 下均无硬错; `pair<int,std::string>` 与 g++ 逐行一致 (含 size/align/成员访问/structured binding 等)
3. `inline namespace __cxx11` 成员在外层作用域可查找; abi_tag 参与 mangle 与 g++ 交叉 (`nm -C` 比对), 且与 libstdc++ 现有 weak 符号一致
4. SFINAE 与 concepts 语义正确: 候选替换失败=剔除, 不是硬错; 全部不满足才硬错; 对 C++20 `requires` 分支同样成立
5. `--component=libcpp` 通过数达到 14/16, `libcpp.a` 由 dcc 产物组成且 `ar`/`readelf` 证明非宿主 cc
6. **无分阶段缺口**: §2 A–L 每一项都在 `feature_matrix.rs` 对应条目翻绿; 不存在"注释里写以后补"; 每一项都有独立夹具与交叉验证

## 7. 回归与风险控制

- 既有测试基线必须保持: lib 1876 / integration 967 / cpp_abi 9 / errors 26 全绿
- 新增分组 `cpp_abi_template/` 触发 `cpp_abi_groups_are_all_covered` 编译期检查; 忘记同步 `COVERED` 常量与分组测试函数会立即失败
- abi_tag 引入的 mangle 变化必须与 libstdc++ 现有 weak 符号 (vtable/typeinfo) 交叉一致, 否则跨 TU 链接期会 undefined
- B4 已建立的 is_complete 门 + namespace 属性容错 + 全局函数指针声明解析是 §2 的前置基础, 不回归
- `--cxx-native=on` 现有钉缺口测试 (`cpp_lang.rs` 中 E0500 断言) 在能力被补上后会**意外通过**, 需同步更新这些用例的断言方向 (缺口填上不是回归, 是把"已知限制"翻成"已支持")
- Auto 模式下 path decision 从 legacy 翻到 native 会影响 `perf_regression` / `reproducibility` 基线, 需要重跑并核对 (承接 `cpp_native_gcc_convergence_plan.md` §0.1 说明二: 先 build 再 probe, 不能用陈旧二进制)

## 8. 与既有文档关系

- 本方案 = `cpp_native_gcc_convergence_plan.md` §1 L0 硬门槛的**具体展开** (Step 8/9 的语义清单)
- 本方案 = `dcc-compile-gxx-source-plan.md` Phase 1–2 (模板/SFINAE/concepts/重载决议) 的**语义落地方案**
- 承接 `cxx-native-foundation-fix-plan.md` (已完成): B4 不完整类 + `_Float32` + 全局函数指针 + namespace 属性容错 → 本方案的 §2 B/§H 与 pair 具体形态直接依赖之
- §2 里的 inline namespace + abi_tag 步骤 (Step 12) 与 `cpp_native_gcc_convergence_plan.md` 的 **D5** (自由函数 Itanium mangling) **必须合并为同一批**落地 (abi_tag 编码建立在真 Itanium 编码之上)

## 附录 A. 关键 stl_pair.h 形态索引 (供实施对照)

| 位置 | 形态 | 涉及能力 |
|---|---|---|
| 78-161 | C++11/C++17 分支 pair 主体 | A,B,C,D,E,F,G,L |
| 165-171 | C++11 + !concepts 分支 `__pair_base` | B |
| 196-215 | `pair(piecewise_construct_t, tuple<_Args1...>, tuple<_Args2...>)` | A,J |
| 242-506 | C++20 concepts 分支 pair (requires / explicit(bool)) | H,I |
| 507-769 | `!concepts` 分支 pair (C++17 走这条) | G |
| 815-862 | `#if __cpp_lib_three_way_comparison && __cpp_lib_concepts` `operator<=>` | K,H |

## 附录 B. 参考标准条款

- 两阶段查找: [temp.res]/8; 惰性实例化: [temp.inst]/[temp.spec]
- SFINAE: [temp.deduct]/8; 重载决议: [over.match.funcs]
- Concepts: [concepts]; requires 表达式: [expr.prim.req]; 约束子句: [temp.constr]; 约束包含判定: [temp.constr.order]
- Inline namespace: [namespace.inline]/3; abi_tag: Itanium B.6
- `if constexpr`: [stmt.if].2; `explicit(bool)`: [class.ctor].4; `<=>`: [expr.spaceship]
