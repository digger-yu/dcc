# C++ 原生后端实施方案

> 目标: 保留当前 C++ lexer/parser/template 的可用资产, 逐步把主路径从
> `C++ AST -> C 文本 -> C parser/codegen` 重构为
> `C++ AST -> C++ Sema/HIR -> ABI Lowering -> typed IR -> codegen/link`。
> 旧的 C 文本降级路径只作为短期 fallback, 最终删除。

## 1. 背景与问题

当前 C++ 路径的核心策略是: 解析 C++ 后由 `cpp::lower` 输出 C 风格源码,
再写入临时 `.c` 文件并复用 dcc 的 C driver、C parser、codegen 和 link。
这个路径让早期功能可以快速落地, 但 C++ 的真实语义无法长期稳定地压扁
成 C 文本:

- class / 成员函数 / 引用退化可以用 C 表达, 但构造析构时序需要显式语义。
- 虚函数、虚继承、this-adjust、RTTI、dynamic_cast 依赖 ABI 元数据。
- 异常处理需要 landing pad、personality、LSDA、`.eh_frame` 等后端支持。
- 模板需要两阶段查找、依赖名解析、偏特化排序和约束求值。
- 当前 fallback 依赖 parser 成功率阈值和 `strip_cpp_keywords`, 语义不可预测。
- `lower.rs` 同时承担类型推断、布局、运行时前导、表达式输出和兼容补丁,
  模块职责过重。

因此最优解是引入 C++ 专用语义层和 ABI lowering, 让 C++ 语义以 typed IR
形式进入现有后端, 而不是再经过 C 文本二次解析。

## 2. 当前实现基线（2026-09）

本方案需要以当前代码而不是历史目标作为实施边界。当前仓库已经具备
`cpp::lexer`、`cpp::parser`、`sema`、`hir`、`abi_layout`、`lower_ir` 和
`native` 模块，C++ native 路径可以在进程内生成 `ast::Program`，再复用
现有 `ir::lower`、优化器、x86_64 codegen 和 link 流程；该路径不会生成临时
`.c` 文件。

但当前实现仍不是方案中最终的 typed IR 闭环：native lower 的输出仍是 C AST，
`src/cpp/lower.rs` 仍是完整的 legacy C 文本降级实现，`src/cpp/legacy/`
目前只是边界骨架。driver 也仍直接调用 native lowering，尚未真正接入
`off/on/auto` 三态控制、能力判断、可审计 fallback 诊断和路径统计。

当前可被代码与测试共同证明的 native 范围应暂时收敛为：自由函数、普通 class、
简单成员函数、引用参数以及无继承/无虚分派的基础构造析构。非虚继承、虚函数、
虚继承、RTTI、dynamic_cast、异常、复杂模板、namespace、member pointer、
coroutine 和 module 虽有 AST/HIR/ABI 数据结构或 legacy 支持，但不能在能力矩阵
中宣称为端到端 native 支持。特别是 `lower_ir` 对部分 virtual-base adjustment
仍有明确拒绝路径。

因此本次更新后的方案不再把 Stage 0--10 视为可并行完成的承诺，而是先完成
控制平面、legacy 边界、native Stage 3 闭环和 GCC 交叉验证基础，再逐阶段推进
ABI、模板和异常；每个阶段必须同时具备实现、路径断言和 GCC 对照测试后才能
扩大 native 能力声明。

## 3. 更新后的总体架构

```text
.cpp / .cc / .cxx
  -> preproc
  -> cpp::lexer
  -> cpp::parser      // 只负责语法, 产出 C++ AST
  -> cpp::sema        // 名字查找、类型检查、重载、模板语义
  -> cpp::hir         // 语义化 C++ 中间表示
  -> cpp::abi_layout  // Itanium ABI: class/vtable/vbase/RTTI/thunk
  -> cpp::lower_ir    // HIR + ABI descriptor -> typed IR
  -> ir / codegen / link
```

### 2.1 保留资产

- `cpp::lexer`: 继续作为 C++ token 输入层。
- `cpp::parser`: 保留 AST 结构和容错解析, 但逐步去掉 lower 相关启发式。
- `cpp::template`: 短期保留 AST 实例化, 后续迁入 sema 驱动实例化。
- `cpp::mangle`: 保留 Itanium 名字修饰, 扩展 ctor/dtor/thunk/RTTI mangling。
- `codegen/x86_64.rs`: 继续作为主要机器码后端, 但接收明确 IR/ABI 操作。

### 2.2 新增模块

- `src/cpp/sema.rs`
  - 作用域、名字查找、类型补全、访问控制、重载决议。
  - 维护 class/member/function/template symbol table。
  - 生成 HIR, 不输出 C 文本。

- `src/cpp/hir.rs`
  - 定义语义化 C++ 节点。
  - 显式表达 ctor/dtor、member access、base cast、virtual call、throw/catch。

- `src/cpp/abi_layout.rs`
  - Itanium ABI class layout。
  - 计算 field offset、non-virtual base、virtual base、vptr、vtable、RTTI。
  - 生成 thunks 和 this/return adjustment descriptor。

- `src/cpp/lower_ir.rs`
  - 把 HIR 和 ABI descriptor 降到 dcc typed IR。
  - 不生成临时 C 文件。

- `src/cpp/runtime.rs`
  - 管理 C++ runtime symbol glue。
  - 只保留确实需要的 helper, 避免每个 TU 无条件输出大量 typedef。

## 3. 数据结构设计

### 3.1 Symbol 与 Scope

```rust
pub enum CxxSymbolKind {
    Namespace,
    Class,
    Field,
    Method,
    Constructor,
    Destructor,
    Function,
    Variable,
    TypeAlias,
    Template,
    Concept,
}

pub struct CxxSymbol {
    pub id: SymbolId,
    pub name: String,
    pub kind: CxxSymbolKind,
    pub ty: CxxType,
    pub owner: Option<SymbolId>,
    pub access: Access,
    pub linkage_name: Option<String>,
}

pub struct Scope {
    pub parent: Option<ScopeId>,
    pub owner: Option<SymbolId>,
    pub symbols: HashMap<String, Vec<SymbolId>>,
}
```

要求:

- 支持 namespace/class/block/function scope。
- 支持同名 overload set。
- 支持 injected-class-name。
- 支持类外定义 `T C::m()` 绑定回已有 member declaration。
- 支持 using declaration / using namespace 的最小可用实现。

### 3.2 CxxType

```rust
pub enum CxxType {
    Void,
    Bool,
    Integer { bits: u16, signed: bool },
    Float { bits: u16 },
    Pointer(Box<CxxType>),
    LValueRef(Box<CxxType>),
    RValueRef(Box<CxxType>),
    Array(Box<CxxType>, Option<usize>),
    Function { ret: Box<CxxType>, params: Vec<CxxType>, cv: CvQual },
    Class(ClassId),
    Enum(EnumId),
    MemberPointer { class: ClassId, member: Box<CxxType> },
    Dependent(DependentType),
}
```

要求:

- 引用类型不在 sema 里直接退化为指针; 只在 ABI lowering 时决定传参形式。
- cv/ref qualifier 是函数类型的一部分。
- dependent type 在模板实例化前不可提前压扁。

### 3.3 HIR 节点

```rust
pub enum HirExprKind {
    Local(SymbolId),
    FieldAccess { object: HirExprId, field: SymbolId },
    BaseCast { object: HirExprId, from: ClassId, to: ClassId },
    VBaseAdjust { object: HirExprId, from: ClassId, to: ClassId },
    Call { callee: HirCallee, args: Vec<HirExprId> },
    Construct { class: ClassId, place: HirExprId, args: Vec<HirExprId> },
    Destroy { class: ClassId, place: HirExprId, variant: DtorVariant },
    TypeInfoRef(ClassId),
    DynamicCast { object: HirExprId, src: CxxType, dst: CxxType },
    Throw { object: HirExprId, ty: CxxType },
}

pub enum HirCallee {
    Direct(SymbolId),
    Virtual { method: SymbolId, slot: usize, this_adjust: ThisAdjust },
}
```

要求:

- 成员访问、base cast、virtual call 必须保留 C++ 语义, 不提前拼地址字符串。
- 构造/析构作为显式节点, 方便生成 cleanup 和异常路径。

### 3.4 ABI Descriptor

```rust
pub struct ClassAbi {
    pub class_id: ClassId,
    pub size: u64,
    pub align: u64,
    pub fields: Vec<FieldLayout>,
    pub bases: Vec<BaseLayout>,
    pub virtual_bases: Vec<VirtualBaseLayout>,
    pub primary_base: Option<ClassId>,
    pub vptrs: Vec<VPtrLayout>,
    pub vtables: Vec<VTableLayout>,
    pub rtti: RttiLayout,
}

pub struct VTableLayout {
    pub symbol: String,
    pub address_point: i64,
    pub components: Vec<VTableComponent>,
}

pub enum VTableComponent {
    OffsetToTop(i64),
    TypeInfo(String),
    Function { symbol: String, adjustment: ThisAdjust },
    VCallOffset(i64),
    VBaseOffset { target: ClassId, offset: i64 },
}

pub struct ThisAdjust {
    pub non_virtual: i64,
    pub vbase_index: Option<usize>,
}
```

要求:

- ABI layer 是 class layout 的唯一来源。
- lower_ir 只能查询 ABI descriptor, 不能自己重新计算偏移。
- 所有 vtable/vbase/thunk/RTTI symbol 都由 ABI layer 分配。

## 4. 分阶段实施计划

### Stage 0: 基线保护与边界冻结

目标: 在重构前固定当前行为, 防止迁移中回归。

任务:

- 建立 C++ feature matrix: class、member fn、ctor/dtor、template、virtual、EH。
- 为当前已通过 C++ fixture 建立 smoke suite。
- 把 `m26_virtual_inheritance_runtime`、模板、dynamic_cast、initializer_list
  等关键用例标记为迁移保护用例。
- 给 driver 增加内部 capability tracing, 方便知道某个 TU 走 native 还是 legacy。

验收:

- 当前 C/C++ 集成测试不回退。
- 可以用环境变量或 verbose 输出确认 C++ 编译路径。

### Stage 1: 拆分 legacy lower, 行为保持不变

目标: 降低 `lower.rs` 复杂度, 为 native lowering 腾出边界。

任务:

- 从 `lower.rs` 抽出 `legacy_runtime.rs`: typedef、runtime prologue、extern 声明。
- 从 `lower.rs` 抽出 `legacy_layout.rs`: 当前 class layout / virtual base 模拟逻辑。
- 从 `lower.rs` 抽出 `legacy_expr.rs`: C 表达式输出逻辑。
- 保留 public API `Lower::lower_decls`, 确保调用方不变。

验收:

- diff 主要是移动代码, 行为不变。
- 现有 C++ 测试全部通过。

### Stage 2: 引入 sema 与 HIR 骨架

目标: 让 C++ parser 之后有独立语义层。

任务:

- 新增 `cpp::sema` 和 `cpp::hir`。
- 建立 TranslationUnit、Scope、SymbolTable、ClassTable。
- 支持 free function、namespace、class、field、method、ctor/dtor 的声明绑定。
- 支持 `this`、成员访问、类外成员函数定义绑定。
- 支持基础类型转换和函数调用类型检查。

验收:

- 可对简单 C++ TU 生成 HIR dump。
- HIR dump 与 legacy lower 结果能对应。
- 不改变默认编译路径。

### Stage 3: 原生 lower 普通函数和普通 class

目标: 打通第一条 C++ native path。

任务:

- 新增 `cpp::lower_ir`。
- free function 直接 lower 到现有 IR。
- 普通 class 作为 aggregate type 进入 IR。
- 成员函数 lower 为带显式 `this` 的函数。
- 引用参数在 ABI lowering 中变为指针传递, sema 中仍保留引用类型。
- driver 增加 `--cxx-native=auto|off|on` 内部开关。

验收:

- 无继承、无虚函数的 class/member/ctor 基础用例走 native path。
- 旧 legacy path 仍可 fallback。

### Stage 4: 非虚继承与构造析构顺序

目标: 解决 C++ 对象生命周期的显式语义。

任务:

- ABI layer 支持 non-virtual base layout。
- HIR 显式生成 base/member construction sequence。
- HIR 显式生成 reverse-order destruction sequence。
- 支持 delegating ctor 的基础形式。
- 支持 copy/move ctor 的最小匹配。

验收:

- 单继承、多重非虚继承字段访问和 base cast 正确。
- ctor/dtor 顺序测试通过。

### Stage 5: vtable 与普通虚函数

目标: 支持无虚继承场景下的真实 virtual dispatch。

任务:

- ABI layer 生成 primary vptr、vtable、offset-to-top、typeinfo slot。
- 支持 virtual function override slot 分配。
- lower_ir 生成 vptr 初始化。
- virtual call lower 为 vtable load + indirect call。
- 支持 this-adjust thunk 的非虚偏移形式。

验收:

- virtual override、base pointer dispatch、multiple inheritance virtual dispatch 通过。
- 不再依赖 C 文本模拟 vtable。

### Stage 6: 虚继承与 vbase offset

目标: 原生解决当前最脆弱的虚继承地址调整。

任务:

- ABI layer 支持 virtual base graph、primary base 选择、vbase offset component。
- HIR `VBaseAdjust` 通过 ABI descriptor lower。
- 成员访问遇到虚基字段时生成 vbase adjust + field offset。
- base cast / dynamic upcast 使用同一套 `ThisAdjust`。
- 生成必要的 virtual thunk。

验收:

- diamond virtual inheritance 测试通过。
- `cpp_virtual_inheritance_runtime` passes after native path implementation.
- legacy 的 `__vbptr` C 表达式模拟不再是主路径。

### Stage 7: RTTI 与 dynamic_cast

目标: 用 ABI 元数据替换简化 helper。

任务:

- 生成 Itanium `type_info`、class type info、base class array。
- typeid 表达式 lower 为真实 RTTI symbol 引用。
- dynamic_cast lower 为 ABI runtime 调用或内联 fast path。
- 处理 null pointer、upcast、downcast、cross-cast。

验收:

- typeid、dynamic_cast up/down/cross cast 测试通过。
- 简化 `__dcc_typeinfo_t` 仅作为 legacy fallback。

### Stage 8: 模板迁入 sema

目标: 从 AST 文本替换转为语义实例化。`/root/dcc/src/cpp/template.rs` 是
当前事实真源 (AST 文本层),本阶段要把实参推导、偏特化排序、概念求值、
实例化触发、缓存键与 mangle 全部迁入 `cpp::sema`,native 路径不再依赖
字符串替换。`template.rs` 在 Stage 10 之前保留为 fallback。

#### Stage 8.1: 模板声明进入 SymbolTable

目标: 让 `template <...>` 声明携带形参表 / requires 子句 / 偏特化 /
概念体完整进入 `cpp::sema::Sema` 的符号表,不再只停留在 parser 的
`CppDecl::Template` / `CppDecl::PartialSpec` / `CppDecl::Concept` 节点。

任务:

- 在 `cpp::sema::Sema::build_hir_from_decls` 内为 `CppDecl::Template` 调用
  新增 `register_template_decl`,把 `params` / `requires` / 内部 `decl`
  一并登记进 SymbolTable 的 `CxxSymbolKind::Template` 槽,通过
  `Sema::class_scope` / `Sema::add_symbol` 让被命名空间 / 外层类持有的
  形参与实体名都进入同一作用域链。`/root/dcc/src/cpp/native.rs::collect_templates_from_decl`
  已经是这条路径的实质内容,要求把语义作用延伸到 sema 而不是只走到
  `TemplateRegistry`。
- 在 `cpp::sema` 内新增 `register_partial_specialization`,对应
  `CppDecl::PartialSpec { params, pattern_text, decl }`,把形参表 /
  pattern 文本 / 体登记到 `Sema::partials: Vec<PartialSpecEntry>`
  (与 `/root/dcc/src/cpp/template.rs::TemplateRegistry::partials` 同形),
  并对偏特化的形参表与主模板形参表做 arity / `is_pack` 一致性校验。
- 在 `cpp::sema` 内新增 `register_concept`,对应
  `CppDecl::Concept { name, body_text }`,形参表从外层 `Template::params`
  中提取;存储为 `Sema::concepts: HashMap<(ns_prefix, name), ConceptInfo>`
  (`params` / `body` / `anchor`)。
- 修正 `cpp::native::detect_features_from_decl` 中 `CppDecl::Template`
  分支,使其登记到 sema 后不再被路径矩阵算成「未接到 sema」;让
  `CppFeature::Concept` / `CppFeature::PartialSpecialization` 的 native
  支持等级真正落地 (现声称 EndToEnd 但路径上仍是 AST 替换)。
- 把 `cpp::template::TemplateRegistry::register_with_prefix` 与
  `register_partial` / `register_full` 改造成 sema 内部薄封装;native
  路径必须从 `Sema` 取形参与偏特化,`TemplateRegistry` 仅作过渡缓存。

验收:

- `tests/fixtures/cpp/template_specialization.cpp` 在 `--cxx-native=on`
  下编译并跑 `scripts/cpp_abi_gcc_cross_check.sh` 与 g++ 一致。
- `tests/fixtures/cpp/concept.cpp` 走 native path,
  `Sema::lookup_concept("Integral", ["T"])` 在 `register_concept` 之后
  能命中并返回 body。
- `Sema::partials` 中至少有一条 `is_same<T, T>` 偏特化登记;
  `src/cpp/sema.rs` tests 模块新增 `template_decl_registers_partial_and_concept`
  锁定入口。
- `tests/fixtures/cpp/template_arg.cpp` /
  `template_nttp_member_use.cpp` 走 `register_template_decl` 统一登记,
  不再经 `native::collect_templates_from_decl` 旁路。

风险与缓解:

- 风险: `TemplateRegistry` 已是事实真源,搬迁到 sema 时若两份数据不同步,
  native lower 与 sema 会看到两份不同形参表。**缓解**: 先把
  `TemplateRegistry` 改造成 `Sema` 镜像 (构造时把 sema 表刷一遍),老调用点
  保留 deprecation 警告。
- 风险: 裸概念 (`concept X = body_text`) 形参表为空,与标准
  `template <typename T> concept X = body_text` 走两条路径。**缓解**:
  本子阶段仅支持标准形式,裸概念保留为限制。

#### Stage 8.2: 依赖类型与依赖表达式的语义化

目标: `MangleType::Template` 中 `args` 含 `Template` / `TplMember` /
`NtExpr` 的形态完整保留到 sema,依赖名按 [temp.dep] 不在实例化前求值。

任务:

- 在 `cpp::sema` 内新增 `fn is_dependent_type(ty: &MangleType) -> bool`,
  递归覆盖 `Template` / `TplMember` / `NtExpr` / `VariadicPack`;
  非依赖返回 false。配合 `/root/dcc/src/cpp/hir.rs::is_dependent` 使用。
- 在 `cpp::sema::Sema::build_hir_from_decls` 内,将
  `parse_template_arg` 产出的 `MangleType::NtExpr(LNot, [TplMember])` /
  `NtExpr(BinOp(Or), [...])` 完整送入 HIR;
  `/root/dcc/src/cpp/parser.rs::parse_nt_expr_template_arg` 已能产出该形态,
  sema 侧需要保证依赖检查路径完整 (避免 `!is_pointer<T>::value` 被错折)。
- 在 `cpp::template::TemplateRegistry::substitute_type` 中为
  `MangleType::NtExpr` 新增分支:形参列表里出现依赖名时整棵 `NtExpr`
  保留;非依赖情形按 `eval_cpp_const` 折成 `MangleType::NonType(v)`。
  该分支替换前调用 `TemplateRegistry::fold_args`。
- 改造 `cpp::sema::Sema::class_constant_member` (见 `/root/dcc/src/cpp/sema.rs::class_constant_member`),
  对依赖成员访问推迟到实例化期才求值;增加 `is_template_param_context`
  入参,依赖形参下返回 `None` 让后续按 `NtExpr` 保留。
- 修正 `cpp::native::collect_template_refs_from_expr` 的 `TplStaticAccess`
  分支(已有 `done` 短路语义),让依赖名出现在类模板形参替换上下文里时
  不被错误地「已经登记」跳过。

验收:

- `tests/fixtures/cpp_abi_template/dependent_nontype_sfinae.cpp` 中
  `enable_if<is_pointer<T>::value, int>::type` 与
  `enable_if<!is_pointer<T>::value, int>::type` 走 native path 与 g++ 一致。
- `tests/fixtures/cpp_abi_template/trait_constant_in_expression.cpp` 中
  非依赖位置 (`is_pointer<int*>::value`) 走 `class_constant_member` 取值;
  依赖位置 (`is_pointer<T>::value` 出现在 traits 体里) 保持 `NtExpr`
  直到实例化时再求值。
- 单元测试 `is_dependent_type_recursive`:对嵌套 `Template` /
  `NtExpr(LNot, [TplMember])` 返回 true;对 `Int` / `Pointer(Int)` 返回 false。

风险与缓解:

- 风险: `NtExpr` 在 `replace_template_type` / `substitute_type` 里递归
  无上限,依赖名套娃会让表达式树指数级膨胀。**缓解**: 在
  `sema::fold_args` 内沿用 `TemplateRegistry::MAX_FOLD_DEPTH = 8`,
  超出后保留未折形态并报 `NativeError::Unsupported`。
- 风险: `class_constant_member` 的 `is_template_param_context` 误判会
  破坏 traits 的 `value` 折叠。**缓解**: 调用点分两类:实例化前 (替换
  上下文) 走 `is_template_param_context=true` 推迟;实例化后 (闭包体
  已具形为 `T=int*` 的具体类) 走 false 正常取值。该判定由
  `template::substitute_decl` 入口负责。

#### Stage 8.3: 函数模板实参推导

目标: 函数模板调用按 [temp.deduct] 完成推导与候选排序,
`Sema` 持有与 `TemplateRegistry::deduce_args` 等价的实参推导表。

任务:

- 在 `cpp::sema` 内新增
  `Sema::deduce_function_template_args(name, arg_tys) -> Option<Vec<Vec<MangleType>>>`,
  内部委托给 `template::TemplateRegistry::deduce_args`,但用
  `Sema::candidates_named(name)` 替代 `TemplateRegistry::candidates_named`。
- 调用点改造 `cpp::sema::Sema::resolve_call` (现走 `pick_best_candidate`
  + `adl_namespaces_for_type`),对名字命中 `CxxSymbolKind::Template` 的
  候选先做实参推导;推导失败按 [temp.deduct]/8 静默移出候选集,
  与 `signature_is_viable` 同语义。
- `cpp::native::collect_template_refs_from_type` 的 `TplMember` 分支需
  确认形参列表里出现的函数模板也加进实例化工作集。
- 新增 `Sema::resolve_overload_for_template_call`:把推导结果与候选签名
  一起送进 `pick_best_candidate`,按实参类型选最匹配的,把该候选的
  实参列表写回 HIR 节点的 `callee_template_args`。
- 让 `cpp::template::substitute_expr` 的 `TemplateIdent` /
  `TplStaticAccess` 分支不仅在 `native::collect_template_refs_from_expr`
  触发时跑,也能在 sema 解析 `f<T>(x)` 时直接跑。

验收:

- `tests/fixtures/cpp/auto_deduction.cpp` / `auto_deduce.cpp` 在
  `--cxx-native=on` 下与 g++ 一致。
- `tests/fixtures/cpp/template_arg.cpp` 的 `f<int>(x)` 与 `f(x)` 都通过
  类型推导得到 `f<int>` 实例。
- 单元测试 `function_template_deduction_picks_specialization`:
  `vector<int>` 推导到 `T=int`,`vector<int*>` 推导到 `T=int*`,
  不匹配候选静默移除。
- `tests/fixtures/cpp/template_nttp_member_use.cpp` 的 NTTP 调用
  (`f<42>(x)`) 走 sema 推导,不走 `legacy::template_text` 的字符串匹配。

风险与缓解:

- 风险: `pick_best_candidate` 的偏序规则对函数模板候选没有「形参推导
  后签名」概念,需要把候选的形参表替换为推导结果再比。**缓解**:
  在 `resolve_overload_for_template_call` 内构造临时候选列表
  (替换后的形参 / 返回类型),再喂给 `pick_best_candidate`。
- 风险: 推导对默认实参 / 参数包要按 [temp.deduct]/7 / [temp.variadic]。
  **缓解**: 把 `TemplateRegistry::param_names` 改为 `&[String]` 视图,
  避免双源漂移。
- 风险: `f<T>(x)` 与 `f(x)` 在 `MangleType::Template{name, args=[]}` 上
  需走两条不同路径。**缓解**: 在 `resolve_call` 内显式分两支,
  不要让 `args=[]` 静默走 `deduce`,也不要让 `args=[T]` 走 `deduce` 后
  再覆盖。

#### Stage 8.4: 类模板显式与隐式实例化

目标: 类模板在使用点 (成员函数体 / 基类列表 / typedef 目标) 触发
实例化,生成语义级 `Sema::class_symbol`,链接名按
`instantiated_class_name` 一致。

任务:

- `cpp::native::lower_decls_native` Step 0 阶段 (见
  `/root/dcc/src/cpp/native.rs::lower_decls_native`) 保留现有
  `instantiate_templates_from_decls` 调用,但把内部「实例化触发」判定
  升级为 sema 驱动:由 `Sema::instantiation_worklist()` (新增) 返回
  使用点列表,每个使用点一份 `(name, args)`。
- `Sema::instantiation_worklist` 遍历 HIR 中 `HirExprKind::TplStaticAccess`
  / `MangleType::Template` / `MangleType::TplMember` / `MangleType::NtExpr`
  操作数,把每个使用点 (含 ctor-initializer / global var init / expression)
  记入工作集;要求与
  `/root/dcc/src/cpp/native.rs::collect_template_refs_from_decl` /
  `from_type` / `from_expr` / `from_stmt` 覆盖范围一致。
- 类模板实例化入口统一改走 `Sema::instantiate_class(name, args)` (新增),
  内部调用 `TemplateRegistry::instantiate_class` 但把结果登记进
  `Sema::classes` (带 `instantiated_class_name` 命名的
  `CxxSymbolKind::Class` 符号)。
- 隐式实例化按 [temp.inst]: 成员函数 / 静态数据成员 / 嵌套类 / 枚举
  只要被 odr-used 就触发。`Sema::instantiation_worklist` 收集使用点时
  同步调 `Sema::mark_odr_used(symbol)`,odr-used 实体才进工作集。
- `cpp::native::collect_template_refs_from_decl` 改造为
  `collect_use_sites_in_decl`:`init` 表达式或 ctor-initializer 实参里
  出现模板引用时必须注册到 sema 工作集。

验收:

- `tests/fixtures/cpp_abi_template/trait_constant_in_expression.cpp` 的
  `ptr_rank<int>` / `ptr_rank<int*>` 在 sema 工作集里出现两次,对应
  两个独立 `CxxSymbolKind::Class` 符号。
- `tests/fixtures/cpp/template_specialization.cpp` 全特化 `is_integral<int>`
  通过 `Sema::find_full_spec` 命中而非主模板实例化。
- `tests/fixtures/cpp/template_braced_init_var.cpp` /
  `template_nested_call_arg.cpp` / `template_depth.cpp` 在 native path
  下与 g++ 一致 (`scripts/cpp_abi_gcc_cross_check.sh --filter=template_`)。
- 单元测试: `Sema::instantiation_worklist` 对含
  `is_pointer<T>::value` / `enable_if<..., T>::type` /
  `Adapter<int, std::vector>` 三种使用点的 fixture 各计一次工作集条目。

风险与缓解:

- 风险: `instantiate_templates_from_decls` 的「定义锚点」语义只在
  `native::collect_templates` 里建立。**缓解**: 让 `Sema` 复用
  `TemplateRegistry::set_anchor` 的存根 (`HashMap<String, usize>`),
  两边共享同一份 anchor 表。
- 风险: 显式 `extern template class Foo<int>;` 解析未在 sema 中,
  会让 native 多生成一份类体导致链接冲突。**缓解**: 本子阶段仅覆盖
  隐式实例化,显式 `extern template` 保留为 fallback。
- 风险: 实例化类体的别名 `__dcc_template_<name>_<arg>...` 在跨 TU 上
  要保证 canonical。**缓解**: `Sema::instantiate_class` 内调
  `TemplateRegistry::key_for` 校一遍,与 `inst_cache` key 一致,
  不一致时报 `NativeError::Unsupported`。

#### Stage 8.5: 实例化缓存与 canonical type key

目标: 同一组 `(<name>, <args>)` 在同一 TU 内只实例化一次;缓存命中
由 `TemplateRegistry::key_for` 决定。

任务:

- 把 `/root/dcc/src/cpp/template.rs::TemplateRegistry::inst_cache`
  改造为 `Sema::inst_cache: HashMap<String, SemaInstantiatedDecl>`,
  其中 `SemaInstantiatedDecl` 持有 `class_symbol_id` 与 `fn_symbol_id`
  两份指向;类与函数走同一张缓存但不同 key 前缀
  (`class:<key>` vs `fn:<key>`),避免同名模板类与函数撞 key。
- `Sema::instantiate_class` / `instantiate_fn` 入口先查缓存再走
  `TemplateRegistry::instantiate_class` / `instantiate_fn`;命中直接把
  已有符号 id 返回给 `lower_decls_native`。
- `TemplateRegistry::type_key` 与 `key_for` 已经在 canonical key 上工作,
  本阶段要补 unit test 锁住三类易飘形态: `MangleType::NtExpr(LNot, [...])` /
  `MangleType::TplMember{name="iterator_traits", args=[Pointer(Int)], member="value_type"}` /
  `MangleType::VariadicPack(elements=[Int, Long, Class("X")])`,要求它们
  与一次正常推导得到的等价实参列表生成同一 key。
- 在 `lower_decls_native` 的 `inst_order: Vec<String>` 上增加
  `Sema::inst_order`,让 `ModuleLinkagePlan::apply` 与 codegen 段都能按
  「先实例化先发射」的稳定顺序枚举实例。
- 缓存监控: `PathStats` 旁新增 `Sema::cache_hit_count` /
  `cache_miss_count` (仅单元测试可见),由 `tests/template_cache.rs`
  (新增) 跑两次同一 TU 验证第二次 `hit=1 miss=0`。

验收:

- 单元测试 `inst_cache_key_uniqueness`:
  `["vector<int>", "vector<int*>", "vector<long>", "vector<long>",
   "enable_if<is_pointer<int*>::value, int>"]` 五次调用,前四次 key
  各不相同,第三次与第四次 key 完全相等;最后一条与「主模板 +
  全特化」两条都命中不同 key。
- `tests/fixtures/cpp/template_depth.cpp` 的 16 层 `Wrap<Wrap<...>>`
  实例化次数 = 16 (不重复)。
- 现有 `tests/cpp_hir_legacy_correspondence.rs` 的 legacy 对照路径
  不受影响 (缓存只对 native path 生效)。

风险与缓解:

- 风险: `type_key` 的递归结构对 `MangleType::Function` 不打印参数 cv
  限定符,重载解析会撞 key。**缓解**: 本阶段限定实例化缓存范围是
  「模板形参表」,自由函数 overload 缓存不在本阶段。
- 风险: 缓存以 `Sema` 为单位,不在 TU 间共享,导致同一组实例在多个 TU
  各自实例化一次。**缓解**: Stage 8 的可接受行为,跨 TU 共享是
  Stage 10 联动 thinLTO 后的事。
- 风险: `MangleType::NtExpr` 的 canonical key 包含整个子式结构,
  长度可能数百字符。**缓解**: 加最大长度阈值 (与 `MAX_FOLD_DEPTH`
  同档位 8 级),超长时按哈希取短键。

#### Stage 8.6: 偏特化匹配与排序

目标: 偏特化按 [temp.spec.partial.match] 的「模式→形参→实参」三步
匹配,按「更特化者优先」的偏序选出最终模板。

任务:

- 把 `/root/dcc/src/cpp/template.rs::TemplateRegistry::pick_partial`
  (已实现 `specificity` 评分: 精确=3、指针=2、通配=1) 的语义镜像进
  `Sema::pick_partial(name, args) -> Option<(SymbolId, Vec<String>, Vec<MangleType>)>`:
  第一个返回值是偏特化对应的 `CxxSymbolKind::Class` 符号,后两个分别
  是偏特化的形参表与绑定后的实参。
- `pick_partial` 必须支持三类 pattern: `is_same<T, T>` 重复形参 (同名
  形参绑定一致)、`is_pointer<T*>` 指针剥离 (绑 `int*` 时 `T=int`、
  不是 `int*`)、`conditional<false, T, F>` 非类型 pattern (绑 `false`)。
- 排序沿用 `specificity` 评分 (同模板内多个偏特化按 specificity 降序取
  第一名);跨偏特化组的「模板参数推断」次序按
  [temp.spec.partial.order]: 对每对偏特化 `(A, B)`,用 `A` 的形参表
  对 `B` 的实参做一次推导,若成功则 `A` 比 `B` 更特化。
- 在 `Sema::instantiate_class` / `instantiate_fn` 入口先调
  `pick_partial`,命中时走偏特化的 `decl`;不命中走主模板实例化。
- 全特化 (`template<> struct is_integral<int>`) 必须优先于偏特化命中:
  要求 `Sema::find_full_spec` 在 `pick_partial` 之前调用,且全特化精确
  匹配时返回。

验收:

- `tests/fixtures/cpp/template_specialization.cpp` 中
  `is_same<long, long>::value == 1`、
  `is_same<long, int>::value == 0`;
  `is_pointer<int*>` / `is_pointer<int>` 同理。
- `tests/fixtures/cpp_abi_template/dependent_nontype_sfinae.cpp` 的
  `enable_if<true, T>` / `enable_if<false, T>` 在调用点正确选择。
- 单元测试: `Sema::pick_partial` 对同一名字下三个偏特化
  (exact / pointer-pattern / wildcard) 的 specificity 排序;指针模式的
  形参 `T` 必须绑到被指对象 (`int*` → `int`)。

风险与缓解:

- 风险: 跨偏特化组的「模板参数推断」次序判断需两两比较,
  `O(P²)`。**缓解**: 现阶段 P 通常 ≤ 3 (traits 库),不优化。
- 风险: `specificity` 评分对「非类型实参精确匹配」
  (`conditional<false, T, F>`) 要给 `+3` 而非 `+1`,否则会被同特异性但
  更晚登记的通配符压过。**缓解**: 在 `pick_partial` 的
  `parse_non_type_pattern` 分支显式 `+3`,加单元测试锁住。
- 风险: 偏特化不可比 (`T*` vs `const T*`) 当前没有良好降级路径。
  **缓解**: 走 [temp.spec.partial.order]/3 的「不可比则歧义」硬错误,
  sema 报 `NativeError::Unsupported`,Auto 回落 legacy。

#### Stage 8.7: requires / concept 求值

目标: `requires` 子句与 `concept` 体按 [temp.concept] / [temp.req] 在
推导期求值,把不满足约束的候选静默移出候选集。

任务:

- 在 `cpp::sema` 内新增
  `Sema::evaluate_constraint(cond: &str, ctx: &SfinaeCtx) -> Option<bool>`,
  把 `/root/dcc/src/cpp/template.rs::evaluate_constraint` 与
  `expand_concept_refs` 搬进 sema;`SfinaeCtx` 持有形参表
  (`params: Vec<String>`) 与实参表 (`args: Vec<MangleType>`)。
- `register_concept` 时 body_text 存为 `ConceptInfo.body`,
  `evaluate_constraint` 遇到 `ConceptName<args>` 先做
  `expand_concept_refs`,再走表达式求值。
- `TemplateRegistry::resolve_args` 在填充默认形参后调
  `Sema::evaluate_constraint(td.requires.as_deref(), &ctx)`,失败返回
  `None`,让 `instantiate_fn` 静默换下一个候选。
- 依赖形参的 `requires Integral<T>` /
  `enable_if<X::value, int>` 子式不立即求值,推迟到实例化时
  (在 `Sema::instantiate_class` / `instantiate_fn` 内部、
  `substitute_decl` 之后) 再求值一次。本阶段只覆盖非依赖 `requires`
  (`requires sizeof(T) == 4`)。
- `tests/fixtures/cpp/concept.cpp` / `if_constexpr.cpp` 走 native path
  时 `Sema::evaluate_constraint` 必须与 g++ 一致。

验收:

- `tests/fixtures/cpp/concept.cpp` 通过 native path 编译,输出与 g++ 一致。
- `tests/fixtures/cpp/if_constexpr.cpp` 中
  `if constexpr (std::is_integral_v<T>)` 折叠成静态分支。
- `tests/fixtures/cpp/cpp20_features.cpp` (`requires` 子句在
  `enable_if` 之外的写法) 在 native path 通过。
- 单元测试 `Sema::evaluate_constraint`:
  `requires true` / `requires false` / `requires sizeof(T) == 4` 三种
  形态给出正确结果;不存在的概念名返回 `None` (而非 `Some(false)`)。

风险与缓解:

- 风险: 概念体可能调用 `<type_traits>` 头里的别名
  (`integral_constant<bool, true>::value`),求值路径必须能折这种 trait。
  **缓解**: Stage 8.7 把「概念体内 trait 折叠」作为硬要求,只在
  `evaluate_constraint` 一处用 `class_constant_member`,别处不变。
- 风险: 概念嵌套递归深度。**缓解**: 沿用 `MAX_ALIAS_EXPANSION = 8`
  上限,超出按 `None` 处理。
- 风险: `requires { expr; }` 表达式需求与 `requires { typename T::X; }`
  类型需求当前不在覆盖范围。**缓解**: Stage 8.7 只覆盖简单 `requires`
  子句 ([temp.req.simple]),compound requirement 留作 fallback。

### Stage 9: 异常处理

目标: 从 C helper 模拟迁移到 zero-cost EH。

任务:

- IR 增加 landing pad / invoke / resume / cleanup 表达能力。
- link 层支持 `.eh_frame` FDE、LSDA、`.gcc_except_table`。
- 生成 `__gxx_personality_v0` 兼容的 action/type table。
- throw lower 为 `__cxa_allocate_exception` + ctor + `__cxa_throw`。
- catch lower 为 landing pad + `__cxa_begin_catch` / `__cxa_end_catch`。
- destructor cleanup 接入异常边。

验收:

- throw/catch 基础测试通过。
- 栈对象析构 cleanup 在异常路径执行。
- 链接到 libstdc++/libgcc_s 时行为兼容。

### Stage 10: driver 切换与 legacy 删除

目标: native C++ path 成为默认路径。本阶段先把链接名口径与 capability
矩阵收紧,再做 `strip_cpp_keywords` 主路径移除与 legacy 模块 cfg 门控,
最后默认行为切到 native 并跑通所有 C++ fixture。

#### Stage 10.1: 模块导入/导出与普通 TU 链接名口径统一

目标: 模块 TU 的导出 (`_ZN...` mangled) 与非模块 TU 的「后端约定名」
(`Foo_ctor_complete_ii` / `poly`) 在链接期一致,跨 TU 链接时不再
`undefined reference`。

任务:

- 在 `/root/dcc/src/cpp/lower_ir.rs::symbol_link_name` 内对
  `CxxSymbolKind::Class` / `CxxSymbolKind::Function` /
  `CxxSymbolKind::Method` 的 mangled 形态做选择: 模块 TU 走 mangled
  (与 `/root/dcc/src/cpp/modules.rs::add_export_renames` 的 map key 一致),
  非模块 TU 走 `member_func_symbol` / `ctor_variant_name` /
  `dtor_variant_name` 后端约定名。该决策以 `ModuleLinkagePlan::rename`
  中是否含本符号为准——若在改名表里,则按改名表的 mangled 名发射。
- 把 `/root/dcc/src/cpp/modules.rs::add_export_renames` 的反向映射搬进
  `lower_ir::symbol_link_name`: 当前它只正向产出「后端约定名 -> mangled」,
  需要在 native lowering 时按 `if rename.contains_key(name) { mangled }
  else { backend_name }` 二选一。
- 模块 TU 入口的
  `/root/dcc/src/cpp/modules.rs::ModuleLinkagePlan::apply` 保持现状,
  移植到 native path 后确保 `plan.drop_data` / `plan.externalize` /
  `plan.rename` 都被 `lower_ir` 读到;新增 `plan.rename` 的反向查询入口
  `ModuleLinkagePlan::mangled_name_of(backend_name) -> Option<String>`。
- `cpp::modules::mangle_global_variable` (在 `add_export_renames` 的
  `BmfKind::Variable` 分支) 现成可用,本阶段只需把它接到
  `lower_ir::symbol_link_name` 的 `CxxSymbolKind::Variable` 分支
  (当前该分支直接返回 `base = sym.name`,与全局变量 mangle 形式不一致)。
- 把 `tests/fixtures/cpp/modules.cpp` / `modules_bmf.cpp` /
  `modules_odr.cpp` 加入 `scripts/cpp_abi_gcc_cross_check.sh` 的覆盖
  (当前 `find cpp_abi_*/*.cpp` 不包含顶层 `cpp/modules_*.cpp`)。

验收:

- `tests/fixtures/cpp/modules.cpp` (模块 TU 导出) +
  `tests/fixtures/cpp/modules_bmf.cpp` (消费模块) 在 native path 下跨
  TU 链接通过,无 `undefined reference`。
- `tests/fixtures/cpp/modules_odr.cpp` 在 native path 下与 g++ 输出一致。
- 单元测试:
  `ModuleLinkagePlan::mangled_name_of("Rect_ctor_complete_ii")` 返回
  `Some("_ZN4RectC1Ei")`;`mangled_name_of("poly")` 返回
  `Some("_Z4polyv")`。

风险与缓解:

- 风险: 模块 TU 的 mangling 与 GCC 不一致时,链接到 `libstdc++` 的标准
  库符号会失败。**缓解**: 本子阶段只覆盖 dcc 自有模块间的链接一致;
  与 g++ ABI 互通属于 Stage 10 末段 `cpp_abi_gcc_cross_check.sh` 增量。
- 风险: `ModuleLinkagePlan::apply` 当前直接改写 `ast::Program`,
  与 `lower_ir` 串接时若 plan 未算完就 lower,会出现 `rename` 查不到 key。
  **缓解**: 在 `native::lower_decls_native` 的 Step 0 之前插入
  `module_linkage_plan` 调用 (参考
  `/root/dcc/src/cpp/driver.rs::run_cpp_driver` 的现有 hook 点)。
- 风险: `cpp::lower_ir::symbol_link_name` 当前实现对自由函数 overload
  后缀由 `func_symbol_signature` 计算,与模块 TU mangling 形式不同。
  **缓解**: 在模块 TU 上下文中 `func_symbol_signature` 被
  `ModuleLinkagePlan::mangled_name_of` 取代,本阶段为 `symbol_link_name`
  加 `mangled_override: Option<&str>` 入参,覆盖原有签名计算。

#### Stage 10.2: capability detector 收紧与 `can_compile_native` 扩展

目标: 把 Stage 8 完成的所有模板能力
(`CppFeature::{FunctionTemplate, ClassTemplate, PartialSpecialization, Concept}`)
从「声称 EndToEnd」升级为「真的返回 Ok」,并把任何 native lower 期的
拒绝按原因分桶进 fallback 统计。

任务:

- 在 `/root/dcc/src/cpp/feature_matrix.rs::CppFeature::level` 仍然返回
  `EndToEnd` 的同时,给
  `/root/dcc/src/cpp/native.rs::can_compile_native` 加静态能力矩阵的
  「二次校验」: 在 `detect_features` 之后调新函数
  `Sema::can_serve_features(features) -> Result<(), Vec<CppFeature>>`,
  对每个 `EndToEnd` 的 feature 至少跑一个最小 fixture (藏在
  `tests/cpp_native_capability.rs` 新增),失败时把 feature 降级到
  `Parsed` 等级。
- 把 `/root/dcc/src/cpp/path_control.rs::decide` 的 `Auto` 分支与
  `Sema::can_serve_features` 联动: `features` 取 `detect_features` 结果,
  每个低于 `EndToEnd` 的 feature 走 fallback 计数;当前 `decide` 已按
  `f.level() < EndToEnd` 判断,要保证模板特性的 level 与
  `Sema::can_serve_features` 一致。
- 在 `tests/fixtures/cpp/template_arg.cpp` / `concept.cpp` /
  `template_specialization.cpp` / `cpp20_features.cpp` 上各跑一次
  `can_compile_native`,把它们加进
  `tests/driver_cxx_native_cli_contract.rs` 的
  `default_and_on_use_the_native_path` 旁路 (与 `PLAIN_CLASS` 同级)。
- 把 Stage 8 修过的「依赖名代入」 `NtExpr` / `TplMember` 路径暴露为
  `CppFeature::DependentTemplateArg`,独立登记 (不在 `Concept` 里重复),
  让 verbose 看到具体哪种特性导致 fallback。

验收:

- `tests/cpp_native_capability.rs::can_compile_native_accepts_template_specialization`
  跑通 `is_integral<int>` / `is_same<T, T>` /
  `enable_if<true, T>` 等典型 fixture。
- `tests/cpp_native_capability.rs::can_compile_native_rejects_coroutine`
  仍然拒绝 `co_await` (不破坏 Stage 9 的门禁)。
- `tests/fixtures/cpp_abi_template/dependent_nontype_sfinae.cpp` 通过
  `can_compile_native` 的二次校验。

风险与缓解:

- 风险: `can_serve_features` 与 `detect_features` 双层校验会出现
  「detector 说能、serve 说不能」的窗口期,此时 native lower 仍会失败。
  **缓解**: 让 `serve` 校验之前必须跑过对应 fixture 的单元测试 (CI gate),
  校验失败直接挂 `path_control::decide` 强制 Auto 回落 legacy。
- 风险: 新增 `CppFeature::DependentTemplateArg` 会让
  `CppFeature::ALL_FEATURES` 列表与
  `/root/dcc/src/cpp/feature_matrix.rs` 末尾的
  `feature_levels_are_monotonic_and_end_to_end_subset` 测试需同步更新。
  **缓解**: 给新 feature 一个 `level() = Parsed`,放在 `ALL_FEATURES`
  末尾,monotonic 性质不变。

#### Stage 10.3: 路径统计与 verbose summary 收紧

目标: `/root/dcc/src/cpp/path_control.rs::PathStats::summary` 输出的
`native=N legacy=L fallback[reason: count, ...]` 在所有 TU 走 native
时呈现 `native=N legacy=0 fallback[]`。

任务:

- 在 `/root/dcc/src/cpp/path_control.rs::PathStats` 内新增
  `record_unsupported_feature(feature: CppFeature)`,把 `CppFeature` 名
  作为 fallback reason 写进 `fallback_reasons`,与 `record_fallback(&str)`
  共用底层 map。
- `/root/dcc/src/cpp/native.rs::native_supported` 在 `Err` 分支调
  `PathStats::record_unsupported_feature`;调用点位于
  `/root/dcc/src/driver.rs::run_cpp_driver` 的 native reject 分支。
- 把 verbose 输出从 `c++ path summary: native=N legacy=L` 扩展为
  `c++ path summary: native=N legacy=L fallback[feature_a: 3, feature_b: 1]`,
  与 `PathStats::summary()` 的格式完全一致;要求
  `tests/driver_cxx_native_cli_contract.rs::off_forces_legacy_with_visible_decision_and_summary`
  同步更新断言文本。
- 在 `path_control::decide` 的 `Auto` 分支记录「矩阵说不能」的具体
  feature 而不只是 `unsupported features: ...`;
  `fallback_reason()` 输出形如 `Concept@Parsed` (feature 名 + level)。
- 加单元测试 `path_control::PathStats::summary_with_unsupported_features`:
  模拟 `record_native()` × 5 + `record_legacy()` × 1 +
  `record_unsupported_feature(FunctionTemplate)` × 3,
  断言输出 `native=5 legacy=1 fallback[FunctionTemplate: 3]`。

验收:

- `tests/driver_cxx_native_cli_contract.rs::off_forces_legacy_with_visible_decision_and_summary`
  在更新后的断言文本下通过。
- 跑 `tests/fixtures/cpp/` 下所有 `.cpp` (用 `--cxx-native=auto`),
  verbose 输出的 `fallback[]` 为空 (除非显式触发不支持特性)。
- `tests/cpp_path_stats.rs` (新增)
  `record_unsupported_feature_aggregates_by_feature_name`:
  对 `Concept` 记录 2 次、`PartialSpecialization` 记录 3 次,
  `summary()` 输出两条。

风险与缓解:

- 风险: `record_unsupported_feature` 与 `record_fallback` 共用 map,
  调用顺序会影响计数。**缓解**: `record_unsupported_feature` 与
  `record_fallback` 二选一,调用点统一为 `record_unsupported_feature`;
  `record_fallback` 留给非 feature 维度 (`native lowering rejected TU: ...`)。
- 风险: verbose 输出格式变更会破坏现有脚本。**缓解**: `summary()`
  保留 `native=N legacy=L` 前缀不变,仅在有 fallback 时追加 `fallback[...]`。

#### Stage 10.4: 删除 `strip_cpp_keywords` 主路径

目标: `/root/dcc/src/cpp/lower.rs::strip_cpp_keywords` 从默认 lowering
流水线移除;仅作为 Stage 10.6 末段才删除的 debug dump 入口。

任务:

- `/root/dcc/src/cpp/legacy/mod.rs::lower_to_c` 内部实现
  `Lower::with_constexpr_fns + lower_decls + finish` 当前会经过
  `lower.rs::strip_cpp_keywords`。在 `lower_to_c` 内加
  `cfg!(feature = "legacy-strip-debug")` 条件门,默认 `feature = []`,
  主路径不调 `strip_cpp_keywords`。
- 把 `/root/dcc/src/cpp/legacy/decl_cleanup.rs::strip_attributes` /
  `strip_gcc_attributes` 与
  `/root/dcc/src/cpp/legacy/expr.rs::replace_cpp_constructs` 从
  `lower_to_c` 默认路径移除;保留模块供 `legacy-strip-debug` feature 用。
- 同步检查 `/root/dcc/src/cpp/lower.rs` 内 `CPP_FEATURE_KEYWORDS` /
  `CPP_STRIP_KEYWORDS` / `apply_post_lowering_conversions` 是否有被
  `legacy::mod` 之外调用点依赖;若是,保留函数体不动,仅不在
  `lower_to_c` 调用链上挂载。
- 在 `/root/dcc/src/cpp/driver.rs::compile_cpp` 上方加注释:
  「Stage 10: 此函数只在 `legacy-strip-debug` feature 启用时调用,
  默认路径走 native」;保留 `DCC_KEEP_TEMP` 行为。
- 把
  `/root/dcc/src/cpp/legacy/template_text.rs::remove_template_definitions` /
  `replace_template_usages` 标记为 `#[allow(dead_code)]`,并在 docstring
  注明「Stage 10.6 末段删除」。

验收:

- `--features legacy-strip-debug` 时 `tests/legacy_strip_debug_smoke.rs`
  (新增) 通过: `strip_cpp_keywords` 仍能产出去掉 `noexcept` /
  `nullptr` 的 C 文本。
- 默认编译 (无 feature) 下,`strip_cpp_keywords` 不出现在
  `lower_to_c` 调用图上 (`cargo bloat --bin dcc` 或单元测试
  `tests/legacy_strip_not_in_main_path.rs` 检查
  `cpp::legacy::lower_to_c` 内部不调 `strip_cpp_keywords`)。
- `tests/driver_cxx_native_cli_contract.rs::off_forces_legacy_with_visible_decision_and_summary`
  在默认 feature 下走 legacy 仍成功 (off 模式不退化)。

风险与缓解:

- 风险: `strip_cpp_keywords` 与 `cpp::legacy::decl_cleanup::strip_attributes`
  是 legacy 的最后一道兜底,删后某些 GCC 扩展代码
  (`__attribute__((unused))`) 会原样泄漏到 `.c` 文本里触发 `host cc`
  失败。**缓解**: 本阶段只删 `strip_cpp_keywords` 主调用入口,
  函数体保留为 `feature = "legacy-strip-debug"` 内的辅助;
  `can_compile_native` 不通过的 TU 仍可走 `--cxx-native=off` 显式调用。
- 风险: `legacy::expr` 内大量函数依赖 `strip_attributes`
  (如 `replace_cpp_constructs` 的注释说「在 strip_cpp_keywords 的字符级
  处理之前调用」),删 `strip_cpp_keywords` 之后这些 pass 顺序约束失效。
  **缓解**: 本阶段不删 `legacy::expr` 的 pass,只删 `lower_to_c` 内部
  串联调用;`legacy::expr` 模块在 Stage 10.5 末段才整体移除。

#### Stage 10.5: native 单测与 GCC 交叉验证加固

目标: 所有现有 C++ fixture 在 `--cxx-native=on` 下通过;
`scripts/cpp_abi_gcc_cross_check.sh` 全量绿 (含 Stage 8 模板夹具)。

任务:

- 把 `tests/fixtures/cpp_abi_template/` 下的两个夹具加入
  `scripts/cpp_abi_gcc_cross_check.sh` 的覆盖 (脚本当前
  `find cpp_abi_*/*.cpp` 已经包含这两个文件,但
  `tests/cpp_abi_gcc_cross_check.rs::cpp_abi_groups_are_all_covered`
  需校验新分组也登记为 cargo 测试)。
- 把 `tests/fixtures/cpp/template_specialization.cpp` / `concept.cpp` /
  `cpp20_features.cpp` / `auto_deduction.cpp` / `template_arg.cpp`
  加入 `tests/integration.rs` 的 C++ 集成测试列表,断言 native 路径下
  编译并退出码 = 0。
- 在 `tests/driver_cxx_native_cli_contract.rs` 中新增
  `native_path_passes_all_template_fixtures`: 跑
  `tests/fixtures/cpp_abi_template/` 两个夹具 + 上述顶层 fixture,
  断言全部 `native=1 legacy=0 fallback[]`。
- 给 `Sema::evaluate_constraint` / `Sema::pick_partial` /
  `Sema::instantiate_class` 各补 2-3 个单元测试 (参考 Stage 8 验收
  一节),锁住与 g++ 同构的偏序规则。
- 在 `tests/cpp_native_capability.rs` (新增) 中跑 `can_compile_native`
  在所有 `cpp_abi_*` 夹具上,确保矩阵校验通过。
- 把 `cpp::legacy::template_text::remove_template_definitions` 的所有
  调用点 (仅 `legacy::lower_to_c` 内部) 改用
  `#[cfg(feature = "legacy-strip-debug")]` 门住,默认编译不再 link
  该函数。

验收:

- `scripts/cpp_abi_gcc_cross_check.sh` (全量,含 `cpp_abi_template/*`)
  输出 0 fail。
- `tests/integration.rs` 的 cxx-native 集成测试 100% 通过。
- `cargo test --test cpp_abi_gcc_cross_check` 通过。
- 单元测试 `tests/cpp_native_capability.rs` 全部绿。

风险与缓解:

- 风险: `scripts/cpp_abi_gcc_cross_check.sh` 用
  `find cpp_abi_*/*.cpp` 列举夹具,新增顶层 `cpp/*.cpp` 不会被纳入。
  **缓解**: 本阶段只把 Stage 8 关注的 `cpp_abi_template/*` 与现有
  `cpp_abi_*/*` 跑齐;顶层 `cpp/template_*.cpp` 在 Stage 10.5 末段单独
  扩展脚本的 include 列表。
- 风险: 集成测试与 CLI 契约测试各自起 dcc 子进程,跑全套 CI 时间会
  显著增加。**缓解**: 给 fixture 加 `#[ignore]` 标记或在 `tests/`
  顶层加 `#[cfg(feature = "slow-tests")]` 门控;CI 上
  `DCC_RUN_SLOW=1` 才跑全套。
- 风险: 模板夹具与 `g++ -std=c++17` 的产物逐字节比对会因为内部表达
  (命名修饰 / 实例名 hash) 不同导致误报。**缓解**:
  `cpp_abi_gcc_cross_check.sh` 只比 stdout 与退出码,不比产物字节,
  模板夹具天然适配。

#### Stage 10.6: 默认路径切换与最终验收

目标: `--cxx-native=auto` 默认行为保持 (已实现),但在 CLI `--help`
中标注「默认与 auto 等价,Stage 10 默认值为 auto」;删除 legacy C 文本
lower 主路径 (仅保留 `legacy::template_text` 的「按 fixture 文本对照」
参考实现)。

任务:

- `/root/dcc/src/cli.rs::cxx_native_mode` 的 `CxxNativeMode::default()`
  已经是 `Auto`
  (`/root/dcc/src/cpp/feature_matrix.rs::cxx_native_mode_default_is_auto`
  单元测试已锁定),本阶段只需在 `--help` 文本 (见
  `/root/dcc/src/cli.rs`) 把 `auto (default)` 改为
  `auto (default, native C++ path; fallback to legacy if feature matrix says no)`,
  让用户知道 fallback 仍在。
- 删除 `/root/dcc/src/cpp/driver.rs::compile_cpp` 的
  `Lower::with_constexpr_fns + lower_decls + finish` 主调用路径
  (Stage 10.4 已把 `strip_cpp_keywords` 移除);本阶段把整个
  `compile_cpp` 函数标 `#[cfg(feature = "legacy-text-lowering")]`,
  默认编译只导出 `native` 入口。
- 把 `/root/dcc/src/cpp/lower.rs` 整体移入 `legacy/` 子模块 (已经是
  `cpp::legacy::lower_to_c` 的薄封装);`lower.rs` 顶层
  `strip_cpp_keywords` / `apply_post_lowering_conversions` /
  `mangle_to_c` 仅在 `legacy-text-lowering` feature 下编译。
- `cpp::legacy::template_text::remove_template_definitions` 整个模块
  标记 `#[cfg(feature = "legacy-text-lowering")]`;
  `cpp::legacy::{decl_cleanup, expr, layout, runtime, name_flatten, text_scan}`
  同。
- 在 `PathStats::summary` 后追加一行
  `c++ fallback reasons (top 3): feature_X@Parsed: N, feature_Y@Parsed: M`
  (保留上一行 `native=N legacy=L fallback[...]`),让 verbose 一眼能看出
  「为何不退回 legacy」——在 `native=100% legacy=0 fallback[]` 时该行
  不打印。
- `tests/driver_cxx_native_cli_contract.rs::default_and_on_use_the_native_path`
  旁加 `default_mode_is_auto_in_help`: 断言 `--help` 输出含
  `auto (default)`。

验收:

- 默认编译 (无 feature) 下,`cargo build` 产物不再含
  `strip_cpp_keywords` / `remove_template_definitions` 的符号
  (`nm` 检查或 `tests/legacy_strip_not_in_main_path.rs`)。
- `tests/driver_cxx_native_cli_contract.rs` 全套通过
  (含新增 `default_mode_is_auto_in_help`)。
- `tests/integration.rs` 全套通过。
- `scripts/cpp_abi_gcc_cross_check.sh` 全量绿。
- 删 `--cxx-native=off` 跑同一夹具 fallback 数 ≥ 1
  (`legacy` 与 fallback 都非零)。

风险与缓解:

- 风险: `legacy` 模块完全 cfg 掉后,`tests/legacy_smoke.rs` (若新增)
  依赖的函数消失,CI 报错。**缓解**: 把所有依赖 `cpp::legacy::*` 的测试
  同样门控在 `#[cfg(feature = "legacy-text-lowering")]` 下;本子阶段
  同时引入 `legacy-text-lowering` feature 给 release CI 用 (不开),
  nightly CI 开 feature 跑全套。
- 风险: 默认编译路径下用户传入 `--cxx-native=off` 编译 C++ 会因
  `compile_cpp` 不存在而 panic。**缓解**: 在 `driver::run_cpp_driver`
  的 `decision` 分支检查 `cfg!(feature = "legacy-text-lowering")`,
  未启用时把 `--cxx-native=off` 转为诊断错误并退出
  (「legacy backend requires --features legacy-text-lowering」)。
- 风险: `apply_post_lowering_conversions` 与 `mangle_to_c` 被 native
  path 的某些边角代码依赖。**缓解**: 在 native 入口不依赖这两函数
  (验证方法: `grep -r "apply_post_lowering_conversions\|mangle_to_c"
  src/cpp/native.rs src/cpp/sema.rs src/cpp/hir.rs
  src/cpp/lower_ir.rs` 应当返回空);若有依赖,把依赖搬进
  `legacy-text-lowering` 模块。

### 当前阶段状态与进入条件

「已验证」栏的判据是**可复跑的证据**，不是代码存在与否:

- `cargo test` —— 单元测试 + 集成测试 + `tests/cpp_abi_gcc_cross_check.rs`;
- `./scripts/cpp_abi_gcc_cross_check.sh` —— `tests/fixtures/cpp_abi_*/*.cpp` 全部
  与本机 g++ 逐行比对 stdout 与退出码 (布局 / 生命周期 / 重载 / RTTI / EH / 模板特征常量)。

| 阶段 | 状态 | 证据与仍存的缺口 |
|---|---|---|
| Stage 0 | 已完成 | 能力矩阵不变式 + `--cxx-native=off/on/auto` + verbose 路径审计 + 回归基线 |
| Stage 1 | 已完成 | `src/cpp/lower.rs` 从 14235 行降到 ≈7400 行, C 文本主路径的 65 个 pass 按职责搬进 `src/cpp/legacy/`:`text_scan` (字节级扫描原语) / `runtime` (typedef 与运行时符号修补) / `layout` (struct 布局与成员表模拟) / `expr` (表达式与语句形态降级) / `name_flatten` (限定名展平) / `template_text` (模板文本实例化) / `decl_cleanup` (顶层声明区清理)。`Lower::lower_decls` / `legacy::lower_to_c` 入参不变, driver 调用点不变; 搬动后的行为门已验证 (全量测试 + 夹具交叉验证与搬动前一致)。顺带修掉搬动中暴露的一处既存缺陷: `strip_attributes` / `strip_gcc_attributes` 注释承诺保留 `__attribute__((weak))`, 但只比一层括号 -> 规范写法 (双括号) 下从未生效, 现按括号配对再剥一层 (`legacy::decl_cleanup::is_weak_attribute`) |
| Stage 2 | 已完成 | sema → HIR 入口、`DCC_CXX_HIR_DUMP`/`DCC_CXX_ABI_DUMP`/`DCC_CXX_IR_DUMP` 可用; 类外成员函数/构造/析构定义已绑定 (复用同一符号, 不再分裂成两个虚槽); 自由函数重载已补全: 链接名按签名去重 (`symbol_link_name` + `free_function_has_rival`, 否则两份定义得到同名符号)、调用点按实参类型选定候选 (`pick_best_candidate`, 含 ADL 候选集)、纯 ADL 调用可用 (`find_namespace_scope` 不再是恒 None 的桩, 类参数的关联命名空间由 `adl_namespaces_for_type` 沿作用域链取); **与 legacy 的自动对照已落地**: `tests/cpp_hir_legacy_correspondence.rs` 以同一份 AST 里「写了函数体的实体」为基准集, 双向断言 native 登记 / legacy 发出 / legacy 不得多出 native 不认识的普通命名定义 (链接名不直接比: 两侧命名口径尚不一致, 比对轴取「角色 + 实体基名」); 该轮同时修掉两条真缺口: 解析器只认一段限定名 (`ns::Cls::method(` 整条降级 Opaque, 现由 `looks_like_out_of_class_def` 按 [basic.lookup.qual] 逐段走) 与 sema 把 `ns::f(...)` 的命名空间前缀当成类名 (现按 [basic.def.outside] 分流到 `build_out_of_line_ns_free_fn` 并复用命名空间里那条声明的符号), 见夹具 `cpp_abi_lifecycle/namespace_qualified_out_of_line_definitions.cpp` |
| Stage 3 | 已完成 | native lower 端到端 + CLI 三态 (真实命令行契约由 `tests/driver_cxx_native_cli_contract.rs` 钉住); `cpp_abi_*` 夹具 28/28 与 g++ 一致 (`scripts/cpp_abi_gcc_cross_check.sh`) |
| Stage 4 | 已完成 | 单一布局真源 (`sizeof`/填充/虚基提升/primary base 复用); ctor-initializer 保留完整实参列表 (`CppCtorInit`); 隐式构造/析构按成员真实需求合成; 委托构造; 全局对象 `.init_array` + `__cxa_atexit`; **类外构造/析构定义** (`B::B(int a) : x(a) {}` / `B::~B() {}` / `ns::C::C(int)`) 解析并复用同一符号 (解析器按声明符边界停住限定链, 见 `parse_qualified_type_name`); **自动存储期析构**按作用域帧在声明点登记, 块末 / `return` / `break` / `continue` 四条出口共用同一批析构 (见 `sema::DtorFrame`); 类类型局部量的直接初始化不再经过临时量 (避免同一对象析构两次); 见 `cpp_abi_lifecycle` 夹具 (`out_of_line_ctor_dtor`、`automatic_storage_destruction`) |
| Stage 5 | 已完成 | primary/secondary vtable、offset-to-top、非虚偏移 thunk 全部由布局层分配; 见 `cpp_abi_virtual` 夹具 |
| Stage 6 | 已完成 | 虚基 vbtable/chain vftable + this-adjust thunk; `hir::ThisAdjust::vbase_index` 已删除, 虚基调整只以 `VBaseAdjust` 表达, lowering 无拒绝路径 |
| Stage 7 | 已完成 | `typeid` 引用形态 (`const std::type_info&`)、`name()`/`before()`/`operator==`、`dynamic_cast` 命中与落空均与 g++ 一致; 见 `cpp_abi_rtti` 夹具。根因修复: 命名空间按 [namespace.def]/2 重开语义复用同一符号与作用域 |
| Stage 8 | 进行中 | 模板迁入 sema 已展开为子阶段 8.1-8.7 详细计划 (见正文);锚点: `Sema::register_template_decl` / `register_partial_specialization` / `register_concept` / `pick_partial` / `evaluate_constraint` / `instantiation_worklist` / `instantiate_class` / `instantiate_fn` / `is_dependent_type` / `deduce_function_template_args` / `resolve_overload_for_template_call` / `can_serve_features` 12 个新入口对应源码实体。现有能力已具备基础: 类模板**全特化**完整建模 (`CppDecl::FullSpec { specific_args, class: ClassDecl }` —— 特化实参取自名字之后的 `<...>`、特化体自己的基类子句与全部成员种类原样保留、实例名与主模板路径同一命名), `TplMember`(`X<Args>::typedef成员`) 展开为别名目标类型而非臆造的类名; **模板实例的编译期常量**已可按语义取值 (`sema::tpl_instance_constant_value` / `class_constant_member`: 先按实例化名定位已登记的类符号, 本类找不到就沿基类链向上 —— traits 的 `value` 几乎总是从 `true_type`/`false_type` 继承来的), 并且 `Member(Ident(类名), m)` 形式的**限定名**不再被当对象成员访问; 表达式语境的**依赖名替换**已补齐 (`template::substitute_expr` 已有 `TplStaticAccess`/`TemplateIdent` 分支, 于是 `ptr_rank<int>` 与 `ptr_rank<int*>` 读到正确的替换实参), 实例化触发点已覆盖 global var init / ctor-initializer / expression 三种上下文 (`native::collect_template_refs_from_decl`)。仍缺: (8.1) `TemplateRegistry` 改造为 `Sema` 镜像并避免双源漂移; (8.2) `class_constant_member` 增加 `is_template_param_context` 入参防止依赖名被误折, `MangleType::NtExpr` 的 `substitute_type` 分支补齐; (8.3) `Sema::resolve_overload_for_template_call` 候选形参替换后做 `pick_best_candidate`; (8.4) `Sema::instantiation_worklist` 把 ctor-initializer / global var init 上下文纳入 odr-used 工作集; (8.5) 类与函数用同一张 `inst_cache` 但 key 前缀区分 (`class:` / `fn:`); (8.6) 跨偏特化组 `O(P²)` 的"模板参数推断"次序判断 (现仅同组 specificity 评分); (8.7) concept 体 trait 折叠 (`class_constant_member` 调用) 与简单 `requires` 求值。三处 fixture 缺口: `tests/fixtures/cpp/enhanced_sfinae.cpp` 的 `enable_if<is_pointer<T*>::value, int>::type` 偏特化匹配、`!is_pointer<T>::value` 解析期不降级、stdlib 头 `<deque>` (`tests/fixtures/cpp/g11_template_templates.cpp` 缺头) |
| Stage 9 | 已完成 | zero-cost EH 全链路替换 SJLJ: IR 有 `Stmt::Invoke`/`Stmt::EhLandingPad` 与 `InstKind::Invoke{unwind, actions}`; x86_64 后端发 `.gcc_except_table`(LSDA) + `.eh_frame` 的 `zPLR` CIE/FDE (personality = `DW.ref.__gxx_personality_v0`, weak 槽) + `.eh_frame_hdr`, 与 g++ 产物逐字节同构; throw = `__cxa_allocate_exception` + ctor(invoke) + `__cxa_throw`; catch = 落地垫 + selector 分派 + `__cxa_begin_catch`/`__cxa_end_catch`; 清理区间按「构造完成语句 → 正常析构语句」的平铺清理链登记 (语法块边界不可靠: sema 把 `D d(1);` 拆成 Decl+Construct, 而 Destroy 发在外层兄弟语句); `_Unwind_Resume` 需 `lp=0/action=0` 裸区间而 `__cxa_rethrow` 需完整 action 链 (均由 libstdc++ 反汇编 + g++ 产物定案)。证据: `cpp_abi_eh` 夹具 6 个 + 16+ 个 EH 场景与 g++ 逐项一致。旧 `gcc_compat.rs` 的 SJLJ C 运行时已删除 (无任何引用点) |
| Stage 10 | 进行中 | driver 切换与 legacy 删除已展开为子阶段 10.1-10.6 详细计划 (见正文);锚点: `lower_ir::symbol_link_name` 加 `mangled_override` / `ModuleLinkagePlan::mangled_name_of` 反向查询 / `Sema::can_serve_features` 二次校验 / `PathStats::record_unsupported_feature` / `legacy-strip-debug` 与 `legacy-text-lowering` 两组 cargo feature / `cpp::driver::compile_cpp` cfg 门控 / `cpp::legacy::*` 整体 cfg 门控 7 处源码改动。现有能力已具备基础: `--cxx-native=off/on/auto` 三态在 driver 真实落地 (`/root/dcc/src/cpp/driver.rs::run_cpp_driver`), `cpp_abi_*` 28/28 夹具与 g++ 一致 (`scripts/cpp_abi_gcc_cross_check.sh`), CLI 契约由 `tests/driver_cxx_native_cli_contract.rs` 钉住, Stage 9 旧 `gcc_compat.rs` SJLJ C 运行时已删除, `auto` 默认值由 `cxx_native_mode_default_is_auto` 锁住。仍缺: (10.1) `ModuleLinkagePlan::mangled_name_of` 反向查询 + `symbol_link_name` 加 `mangled_override: Option<&str>` 入参, 跨 TU 模块导出/导入的链接冲突实例如 `tests/fixtures/cpp/modules.cpp` + `modules_bmf.cpp` + `modules_odr.cpp` 仍未纳入 `cpp_abi_gcc_cross_check.sh`; (10.2) `Sema::can_serve_features` 二次校验与 `CppFeature::DependentTemplateArg` 新增; (10.3) `PathStats::record_unsupported_feature` 与 verbose `fallback[feature@level: count]` 扩展; (10.4) `strip_cpp_keywords` 主调用入口从 `cpp::legacy::lower_to_c` 移除, 改用 `feature = "legacy-strip-debug"` 门控; (10.5) `cpp_abi_template/*` 两个夹具进 cross-check, `tests/cpp_native_capability.rs` 新增; (10.6) `cpp::legacy::{template_text, decl_cleanup, expr, layout, runtime, name_flatten, text_scan}` 整体 `#[cfg(feature = "legacy-text-lowering")]`, `--cxx-native=off` 在该 feature 未启用时转诊断错误 |

## 6. 已知缺陷 (与分阶段目标正交的既存问题)

- **静态成员函数在 native 路径上不可用**: `struct S { static int f(); }; int main(){ return S::f(); }`
  报 `member function call missing this argument`。根因是「静态性」根本没进语义层:
  parser 的 `CppDecl::MemberFn { is_static }` 在 `sema::CxxSymbol` / `hir` / `lower_ir` 全链上
  没有对应位 (与 `is_const`/`is_volatile` 当年一样的缺口), 所以 `S::f` 既不能被识别为限定名
  常量访问, 也不能被发出为不带 `this` 的直接调用。跨层补 `is_static` 是修复口径,
  按调用点特判不是。
- **清理动作里二次抛出按 `std::terminate`**: 析构函数自己抛出时, GCC 会在同一个
  帧里再走一次展开 (把新异常交给更外层); dcc 的清理链目前不重入 LSDA, 因此该场景
  按 terminate 处理。`eh_cleanup_dtor_order_across_frames` 只覆盖不抛出的析构。
- **全表达式临时量不参与异常清理**: 清理区间按语句定界 (见 Stage 9 行), 因此同一条
  语句里先构造后抛出的中间临时量 (如 `f(T(), g())` 里 `T` 已构造完而 `g()` 抛出)
  不会被析构; GCC 会按完整表达式边界登记多个区间。
- **构造函数部分构造失败的清理**: 基类/成员构造中途抛出时, GCC 会调用本类已
  构造完那部分的析构并把异常继续外递; dcc 目前只在整体构造点位登记区间。
- **`char` / `signed char` 在链接名签名里不可区分** (`overload_signature` 给三者同一
  类型码), 仅靠 `char` vs `signed char` 区分的重载仍会归到同一个链接名。
- **有副作用的返回表达式与析构顺序**: `lower_ir` 把 `Return(expr)` 的求值排在同一块内
  后续语句之后, 因此 `return p->area();` 这类「返回值来自对局部对象的调用」不能提前
  析构 (否则读到被析构改写过的 vptr)。sema 侧只对**无副作用且不从类对象取值**的返回
  表达式发射返回前析构 (见 `expr_is_side_effect_free`), 其余情形维持既存行为。

已修复并从本节移除: `if (c) { stmt; break; }` 在循环体内丢失分支语句的块归属问题
(SSA→AST 重建中「循环体外目标块」现在按 `should_inline_loop_exit_target` 判定就地
展开, 共享出口块仍按 `break` 处理), 见 `cpp_abi_lifecycle/break_after_statement_in_loop.cpp`。

阶段推进必须遵守单向门禁：先完成当前阶段的实现、单元测试、端到端路径
断言、legacy 对照和 GCC 交叉验证，再修改 `native_supported()` 或扩大
capability detector。任何失败都必须返回结构化的 unsupported reason；`On`
不得静默 fallback，`Auto` 的 fallback 必须在 verbose 输出中可见。

## 5. Driver 改造方案

### 5.1 短期 driver

```rust
match cxx_native_mode {
    Off => legacy_lower_to_c(),
    On => native_cxx_to_ir_or_error(),
    Auto => native_cxx_to_ir().or_else(|reason| legacy_lower_to_c_with_diag(reason)),
}
```

要求:

- `On` 模式不允许 silent fallback, 便于测试 native 覆盖率。
- `Auto` 模式保留兼容性。
- verbose 输出包含 path、fallback reason、unsupported feature。

### 5.2 临时文件策略

- native path 不写临时 `.c`。
- legacy path 保留 `DCC_KEEP_TEMP` 便于排错。
- 增加 `DCC_CXX_HIR_DUMP`、`DCC_CXX_ABI_DUMP`、`DCC_CXX_IR_DUMP`。

## 6. IR 与后端改造

### 6.1 必要 IR 能力

- typed aggregate address。
- explicit pointer adjustment。
- indirect call with adjusted this。
- global constant objects for vtable / RTTI。
- landing pad / cleanup edge。
- COMDAT / weak ODR linkage metadata。

### 6.2 x86_64 后端要求

- vtable/RTTI symbol 进入 `.rodata` 或 COMDAT section。
- thunk 作为真实 function symbol 输出。
- PC-relative relocation 复用现有 relocation 机制。
- member function pointer 按 Itanium layout 编码。
- exception table 通过 link 层输出, codegen 只记录 landing pad metadata。

## 7. 测试计划

### 7.1 单元测试

- sema scope lookup。
- overload resolution。
- class layout。
- vtable slot 分配。
- vbase offset 计算。
- template canonical key。
- RTTI graph 生成。

### 7.2 集成测试

- 普通 class/member/ctor/dtor。
- non-virtual inheritance。
- multiple inheritance。
- virtual dispatch。
- diamond virtual inheritance。
- base pointer cast / member access。
- typeid / dynamic_cast。
- throw/catch / destructor cleanup。
- function template / class template / partial specialization。
- initializer_list。
- static local initialization guard。

### 7.3 兼容性测试

- 与 g++ 对比 class size/align/field offset。
- 与 g++ 对比 vtable dump。
- 与 g++ 对比 mangled symbol。
- 编译 GCC bootstrap 相关最小样本。

## 8. 迁移风险与缓解

- **风险: 一次性替换路径导致大量回归**
  - 缓解: auto/on/off 三态开关, native 按 feature 分阶段接管。

- **风险: ABI layout 与 g++ 不一致**
  - 缓解: 添加 g++ layout/vtable 对照测试, ABI descriptor 单元测试先行。

- **风险: 模板语义复杂度失控**
  - 缓解: 先支持非依赖和常见依赖场景, 复杂 SFINAE 保留 fallback。

- **风险: 异常处理牵动 link/codegen 大改**
  - 缓解: Stage 9 独立推进, 在此前 throw/catch 继续 legacy/error gate。

- **风险: runtime helper 与系统 libstdc++ 符号冲突**
  - 缓解: native path 优先声明外部 ABI symbol, dcc helper 只用于 freestanding 或 legacy。

## 9. Definition of Done

- C++ 默认路径不再生成临时 C 源文件。
- 当前所有 C++ 集成测试 native path 通过。
- `cpp_virtual_inheritance_runtime` does not rely on `__vbptr` C expression simulation.
- class layout、vtable、RTTI 与 g++ 对照测试通过。
- 模板基础场景由 sema 实例化, 不依赖字符串替换。
- throw/catch 基础场景可链接并运行。
- legacy lower 仅保留为 debug/fallback, 并有清晰删除计划。
