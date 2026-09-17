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

## 2. 总体架构

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
- `m26_virtual_inheritance_runtime` 走 native path 后通过。
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

目标: 从 AST 文本替换转为语义实例化。

任务:

- 模板声明进入 SymbolTable。
- 支持 dependent type / dependent expr。
- 实现函数模板实参推导。
- 实现类模板显式实例化和隐式实例化。
- 实现偏特化匹配和排序的最小完整集。
- requires/concept 在 sema 中求值。

验收:

- 当前模板 fixture 走 sema 实例化后通过。
- 模板实例化缓存按 canonical type key 命中。
- legacy AST 替换作为 fallback 保留一段时间。

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

目标: native C++ path 成为默认路径。

任务:

- `--cxx-native=auto` 默认打开。
- capability detector 判断当前 TU 是否可 native 编译。
- auto 模式下不可支持特性才 fallback legacy。
- 为 fallback 输出明确 diagnostic。
- native 覆盖全部当前 C++ 测试后, 删除 `strip_cpp_keywords` 主路径。
- 最终删除 legacy C 文本 lower 或仅保留 debug dump。

验收:

- 当前 C++ tests 全部 native 通过。
- 不再生成临时 `.c` 作为默认路径。
- fallback 数量在 verbose summary 中为 0。

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
- `m26_virtual_inheritance_runtime` 不依赖 `__vbptr` C 表达式模拟。
- class layout、vtable、RTTI 与 g++ 对照测试通过。
- 模板基础场景由 sema 实例化, 不依赖字符串替换。
- throw/catch 基础场景可链接并运行。
- legacy lower 仅保留为 debug/fallback, 并有清晰删除计划。
