//! C++ feature matrix: 枚举 C++ 前端支持的能力
//!
//! 用于:
//! - capability tracing: 记录某个 TU 走 native 还是 legacy 路径
//! - auto mode: 判断是否可以走 native path
//! - smoke test: 标记迁移保护用例
//!
//! 能力按方案文档的 Stage 顺序排列，逐步从 legacy 迁移到 native。
//!
//! ## 多档支持级别 (Stage 0)
//!
//! 每个特性分为四档支持度，反映"该特性已经走到哪一步":
//!
//! - `parsed`         : 解析器能识别 (不会 E0001)
//! - `sema_resolved`  : 名字查找 / 重载解析通过 (不会 E0xxx 名字类错误)
//! - `lowered`        : 已经被 lowering 产出 IR (函数体已被翻译)
//! - `end_to_end`     : native 路径端到端产出与 GCC 一致的目标文件
//!   (Auto 模式下此档会走 native)
//!
//! `native_supported()` 历史上等价于"end_to_end_supported"——为避免
//! 在 Stage 0 一次性收紧该断言后产生大量依赖该语义的回归,
//! 这里保持 `native_supported()` 含义不变, 但额外提供 `level()` 与
//! `end_to_end_supported()` 给 driver 与测试使用.

/// C++ 特性能力标识。每个能力对应方案中一个迁移里程碑。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CppFeature {
    // --- Stage 3: 普通 class / 函数 (端到端 native 默认候选) ---
    /// 自由函数（无类，无模板）
    FreeFunction,
    /// 普通 class (字段 + 方法, 无继承, 无虚函数)
    PlainClass,
    /// 成员函数（带 this）
    MemberFunction,
    /// 构造函数 / 析构函数（非继承场景）
    CtorDtor,
    /// 引用类型（T&，ABI 降级为指针）
    ReferenceType,

    // --- Stage 4: 非虚继承 ---
    /// 单继承 / 多重非虚继承
    NonVirtualInheritance,
    /// 基类构造顺序
    BaseCtorOrder,
    /// 构造委托
    DelegatingCtor,

    // --- Stage 5: 虚函数 ---
    /// 虚函数 / vtable（无虚继承）
    VirtualFunction,
    /// vptr 初始化
    VPtrInit,
    /// this-adjust thunk（非虚偏移）
    ThisAdjustThunk,

    // --- Stage 6: 虚继承 ---
    /// 虚继承 / vbase offset
    VirtualInheritance,
    /// diamond 虚继承
    DiamondVirtualInheritance,

    // --- Stage 7: RTTI ---
    /// typeid / type_info
    TypeId,
    /// dynamic_cast（upcast / downcast / cross-cast）
    DynamicCast,

    // --- Stage 8: 模板 ---
    /// 函数模板实例化
    FunctionTemplate,
    /// 类模板实例化
    ClassTemplate,
    /// 模板偏特化
    PartialSpecialization,

    // --- Stage 9: 异常 ---
    /// throw / catch
    Exception,
    /// 栈对象析构 cleanup（异常路径）
    DtorCleanup,

    // --- 其他高级特性（长期目标） ---
    /// namespace
    Namespace,
    /// operator 重载
    OperatorOverload,
    /// new / delete
    NewDelete,
    /// std::initializer_list
    InitializerList,
    /// 静态局部变量 init guard
    StaticInitGuard,
    /// 协程
    Coroutine,
    /// concept / requires
    Concept,
    /// module
    Module,
    /// 依赖名代入 (Stage 8 修过的 `NtExpr` / `TplMember` 路径):
    /// 形如 `enable_if<is_pointer<T>::value, int>::type` /
    /// `TplStaticAccess<T>` 等需要在实例化期把模板形参替换进表达式
    /// 与类型表达式的情形. 独立登记以便 verbose 看到具体哪种特性
    /// 导致 fallback, 而不与 `Concept` 混在一起.
    DependentTemplateArg,
}

/// C++ 特性当前所达到的支持档位 (Stage 0 多档支持矩阵).
///
/// 各档是单调升级的:
/// `Parsed` ≤ `SemaResolved` ≤ `Lowered` ≤ `EndToEnd`.
///
/// 升级由原生后端 Stage 推进推进; 当前真实状态由单元测试锁定.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CppSupportLevel {
    /// 仅被 lexer/parser 识别, 未做语义 / 降级
    Parsed,
    /// 通过 Sema (名字查找, 类型检查)
    SemaResolved,
    /// 已能产出 HIR / IR (lower 链路通畅)
    Lowered,
    /// native 端到端产出与 GCC 一致的目标文件
    EndToEnd,
}

impl CppSupportLevel {
    pub fn name(self) -> &'static str {
        match self {
            Self::Parsed => "Parsed",
            Self::SemaResolved => "SemaResolved",
            Self::Lowered => "Lowered",
            Self::EndToEnd => "EndToEnd",
        }
    }
}

impl CppFeature {
    /// Stage 10.3: 特性对应的 **PascalCase 变体名** (`FunctionTemplate` /
    /// `PartialSpecialization` / `DependentTemplateArg` 等), 用于
    /// `feature@level` 形式的 fallback 聚合键, 与 `verbose --features=class`
    /// 等已有口径保持一致。
    pub fn variant_name(&self) -> &'static str {
        match self {
            Self::FreeFunction => "FreeFunction",
            Self::PlainClass => "PlainClass",
            Self::MemberFunction => "MemberFunction",
            Self::CtorDtor => "CtorDtor",
            Self::ReferenceType => "ReferenceType",
            Self::NonVirtualInheritance => "NonVirtualInheritance",
            Self::BaseCtorOrder => "BaseCtorOrder",
            Self::DelegatingCtor => "DelegatingCtor",
            Self::VirtualFunction => "VirtualFunction",
            Self::VPtrInit => "VPtrInit",
            Self::ThisAdjustThunk => "ThisAdjustThunk",
            Self::VirtualInheritance => "VirtualInheritance",
            Self::DiamondVirtualInheritance => "DiamondVirtualInheritance",
            Self::TypeId => "TypeId",
            Self::DynamicCast => "DynamicCast",
            Self::FunctionTemplate => "FunctionTemplate",
            Self::ClassTemplate => "ClassTemplate",
            Self::PartialSpecialization => "PartialSpecialization",
            Self::Exception => "Exception",
            Self::DtorCleanup => "DtorCleanup",
            Self::Namespace => "Namespace",
            Self::OperatorOverload => "OperatorOverload",
            Self::NewDelete => "NewDelete",
            Self::InitializerList => "InitializerList",
            Self::StaticInitGuard => "StaticInitGuard",
            Self::Coroutine => "Coroutine",
            Self::Concept => "Concept",
            Self::Module => "Module",
            Self::DependentTemplateArg => "DependentTemplateArg",
        }
    }

    /// 特性对应的人类可读名称
    pub fn name(&self) -> &'static str {
        match self {
            Self::FreeFunction => "free function",
            Self::PlainClass => "plain class",
            Self::MemberFunction => "member function",
            Self::CtorDtor => "constructor/destructor",
            Self::ReferenceType => "reference type",
            Self::NonVirtualInheritance => "non-virtual inheritance",
            Self::BaseCtorOrder => "base constructor order",
            Self::DelegatingCtor => "delegating constructor",
            Self::VirtualFunction => "virtual function",
            Self::VPtrInit => "vptr initialization",
            Self::ThisAdjustThunk => "this-adjust thunk",
            Self::VirtualInheritance => "virtual inheritance",
            Self::DiamondVirtualInheritance => "diamond virtual inheritance",
            Self::TypeId => "typeid",
            Self::DynamicCast => "dynamic_cast",
            Self::FunctionTemplate => "function template",
            Self::ClassTemplate => "class template",
            Self::PartialSpecialization => "partial specialization",
            Self::Exception => "exception (throw/catch)",
            Self::DtorCleanup => "destructor cleanup",
            Self::Namespace => "namespace",
            Self::OperatorOverload => "operator overload",
            Self::NewDelete => "new/delete",
            Self::InitializerList => "initializer_list",
            Self::StaticInitGuard => "static init guard",
            Self::Coroutine => "coroutine",
            Self::Concept => "concept",
        Self::Module => "module",
        Self::DependentTemplateArg => "dependent template arg",
        }
    }

    /// 当前特性达到的支持档位.
    ///
    /// 当前基准反映 Stage 3 完成的最小端到端子集 (plain class / free
    /// function / member function / ctor/dtor / reference type) 与 Stage 9
    /// 完成的 throw/catch (zero-cost EH: invoke + landing pad + LSDA + 表驱动
    /// 展开)。其他特性按"已解析但未端到端验证"标记为 `Parsed`/`Lowered`/...,
    /// 由 Stage 推进逐档升级。
    ///
    /// 注意: Auto 模式仅在 `EndToEnd` 档特性上才走 native; 其它档位触发
    /// fallback, 由 driver 的 capability tracing 记录.
    pub fn level(&self) -> CppSupportLevel {
        use CppSupportLevel as Lvl;
        match self {
            // Stage 3: 已端到端验证
            Self::FreeFunction => Lvl::EndToEnd,
            Self::PlainClass => Lvl::EndToEnd,
            Self::MemberFunction => Lvl::EndToEnd,
            Self::CtorDtor => Lvl::EndToEnd,
            Self::ReferenceType => Lvl::EndToEnd,

            // 已有 native lowering 与集成测试覆盖的能力. Stage 0 保持这些
            // 能力的默认路径不变, 避免 Auto 因矩阵细化而回退到不兼容的
            // legacy C 文本路径.
            Self::NonVirtualInheritance
            | Self::BaseCtorOrder
            | Self::DelegatingCtor
            | Self::VirtualFunction
            | Self::VPtrInit
            | Self::ThisAdjustThunk
            | Self::VirtualInheritance
            | Self::DiamondVirtualInheritance
            | Self::TypeId
            | Self::DynamicCast
            | Self::FunctionTemplate
            | Self::ClassTemplate
            | Self::PartialSpecialization
            | Self::DtorCleanup
            | Self::Namespace
            | Self::OperatorOverload
            | Self::NewDelete
            | Self::InitializerList
            | Self::StaticInitGuard
            | Self::Concept
            // 协程由 `coroutine_lower` 在 native 管线之前重写为普通函数 +
            // 状态机; 模块由 driver 的 BMF 接口/导入 splice 后走同一条
            // native 管线, 两者都有端到端集成测试 (Stage 0 矩阵)。
            | Self::Coroutine
            | Self::Module => Lvl::EndToEnd,

            // Stage 9: 异常端到端已通 (zero-cost EH, 与 g++ 产物交叉验证)
            Self::Exception => Lvl::EndToEnd,

            // Stage 10.2: 依赖名代入 (NtExpr / TplMember 替换路径) 已能
            // 在 sema / template 管线产出替换后的实参, 但实例化期还需
            // Sema::can_serve_features 二次校验. 落到 `Parsed` 档以让
            // monotonic 测试不被破坏, verbose 仍能看到具体哪种 fallback
            // (Stage 10.3 的 `record_unsupported_feature` 会读取这里).
            Self::DependentTemplateArg => Lvl::Parsed,
        }
    }

    /// 端到端支持 (Auto 模式据此判断走 native 还是 fallback)
    pub fn end_to_end_supported(&self) -> bool {
        self.level() == CppSupportLevel::EndToEnd
    }

    /// 当前 native path 已支持的特性集合.
    ///
    /// 保留历史语义: 这是 native lowering 能处理的能力集合, 不等同于
    /// Auto 模式的端到端门槛. Driver 使用 `end_to_end_supported()` 决定
    /// Auto 是否切换 native, 而 native 单元测试继续使用该 API 验证
    /// lowering 覆盖面.
    pub fn native_supported(&self) -> bool {
        matches!(
            self,
            CppFeature::FreeFunction
                | CppFeature::PlainClass
                | CppFeature::MemberFunction
                | CppFeature::CtorDtor
                | CppFeature::ReferenceType
                | CppFeature::NonVirtualInheritance
                | CppFeature::BaseCtorOrder
                | CppFeature::DelegatingCtor
                | CppFeature::VirtualFunction
                | CppFeature::VPtrInit
                | CppFeature::ThisAdjustThunk
                | CppFeature::VirtualInheritance
                | CppFeature::DiamondVirtualInheritance
                | CppFeature::TypeId
                | CppFeature::DynamicCast
                | CppFeature::FunctionTemplate
                | CppFeature::ClassTemplate
                | CppFeature::Exception
                | CppFeature::DtorCleanup
                | CppFeature::NewDelete
                | CppFeature::InitializerList
                | CppFeature::Namespace
                | CppFeature::OperatorOverload
                | CppFeature::StaticInitGuard
                | CppFeature::PartialSpecialization
                | CppFeature::Concept
                | CppFeature::Coroutine
                | CppFeature::Module
        )
    }

    /// 能力矩阵的全量特性清单, 供测试与 verbose 诊断遍历使用.
    ///
    /// 新增特性时必须同步这里, 矩阵不变式测试 (`feature_levels_are_monotonic_*`)
    /// 才能覆盖到全部档位.
    pub const ALL_FEATURES: &[CppFeature] = &[
        CppFeature::FreeFunction,
        CppFeature::PlainClass,
        CppFeature::MemberFunction,
        CppFeature::CtorDtor,
        CppFeature::ReferenceType,
        CppFeature::NonVirtualInheritance,
        CppFeature::BaseCtorOrder,
        CppFeature::DelegatingCtor,
        CppFeature::VirtualFunction,
        CppFeature::VPtrInit,
        CppFeature::ThisAdjustThunk,
        CppFeature::VirtualInheritance,
        CppFeature::DiamondVirtualInheritance,
        CppFeature::TypeId,
        CppFeature::DynamicCast,
        CppFeature::FunctionTemplate,
        CppFeature::ClassTemplate,
        CppFeature::PartialSpecialization,
        CppFeature::Exception,
        CppFeature::DtorCleanup,
        CppFeature::Namespace,
        CppFeature::OperatorOverload,
        CppFeature::NewDelete,
        CppFeature::InitializerList,
        CppFeature::StaticInitGuard,
        CppFeature::Coroutine,
        CppFeature::Concept,
        CppFeature::Module,
        CppFeature::DependentTemplateArg,
    ];
}

/// capability tracing 结果：某个 TU 编译时使用的路径
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CppPath {
    /// C 文本降级路径（legacy）
    Legacy,
    /// C++ 原生语义路径（native）
    Native,
}

impl CppPath {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Legacy => "legacy (C-text lowering)",
            Self::Native => "native (HIR → IR)",
        }
    }
}

/// C++ native 模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CxxNativeMode {
    /// 始终走 legacy 路径
    Off,
    /// 始终走 native 路径，不支持的特性报错
    On,
    /// 优先 native，不支持的特性自动 fallback 到 legacy
    #[default]
    Auto,
}

impl CxxNativeMode {
    /// 从字符串解析
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "off" | "0" => Some(Self::Off),
            "on" | "1" => Some(Self::On),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }
}

/// 标准库 `FromStr` 入口: 与 [`CxxNativeMode::parse`] 同一映射,
/// 但把无法识别的文本作为 `Err(String)` 回传 (便于 CLI 报错原文).
impl std::str::FromStr for CxxNativeMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s).ok_or_else(|| format!("unknown C++ native mode: {s}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_names_are_unique() {
        use std::collections::HashSet;
        let all = [
            CppFeature::FreeFunction,
            CppFeature::PlainClass,
            CppFeature::MemberFunction,
            CppFeature::CtorDtor,
            CppFeature::ReferenceType,
            CppFeature::NonVirtualInheritance,
            CppFeature::BaseCtorOrder,
            CppFeature::DelegatingCtor,
            CppFeature::VirtualFunction,
            CppFeature::VPtrInit,
            CppFeature::ThisAdjustThunk,
            CppFeature::VirtualInheritance,
            CppFeature::DiamondVirtualInheritance,
            CppFeature::TypeId,
            CppFeature::DynamicCast,
            CppFeature::FunctionTemplate,
            CppFeature::ClassTemplate,
            CppFeature::PartialSpecialization,
            CppFeature::Exception,
            CppFeature::DtorCleanup,
            CppFeature::Namespace,
            CppFeature::OperatorOverload,
            CppFeature::NewDelete,
            CppFeature::InitializerList,
            CppFeature::StaticInitGuard,
            CppFeature::Coroutine,
            CppFeature::Concept,
            CppFeature::Module,
        ];
        let mut seen = HashSet::new();
        for f in &all {
            assert!(seen.insert(f.name()), "duplicate feature name: {}", f.name());
        }
    }

    #[test]
    fn cxx_native_mode_default_is_auto() {
        assert_eq!(CxxNativeMode::default(), CxxNativeMode::Auto);
    }

    #[test]
    fn cxx_native_mode_from_str() {
        assert_eq!(CxxNativeMode::parse("off"), Some(CxxNativeMode::Off));
        assert_eq!(CxxNativeMode::parse("on"), Some(CxxNativeMode::On));
        assert_eq!(CxxNativeMode::parse("auto"), Some(CxxNativeMode::Auto));
        assert_eq!(CxxNativeMode::parse("bogus"), None);
        // FromStr 入口与 parse 保持同一映射
        assert_eq!("1".parse::<CxxNativeMode>().unwrap(), CxxNativeMode::On);
        assert!("bogus".parse::<CxxNativeMode>().is_err());
    }

    #[test]
    fn feature_levels_are_monotonic_and_end_to_end_subset() {
        // 端到端支持的特性集合必须只来自 EndToEnd 档.
        // 这个测试锁死 Stage 0 能力矩阵的对外契约.
        // Stage 0 的端到端基线由 level() 唯一决定，测试显式检查矩阵中所有
        // EndToEnd 特性，而不是维护一份容易过期的子集列表.
        // 矩阵契约: 任何声称 EndToEnd 的特性都必须能被 native lowering
        // 处理, 否则 Auto 模式会把 TU 交给无法表达该语义的路径.
        // 这里遍历 *全部* 特性而不是维护一份容易过期的清单.
        for f in ALL {
            let lvl = f.level();
            assert!(
                lvl <= CppSupportLevel::EndToEnd,
                "feature {:?} level out of range: {:?}",
                f,
                lvl
            );
            if f.end_to_end_supported() {
                assert!(
                    f.native_supported(),
                    "feature {:?} is EndToEnd but native lowering does not support it",
                    f
                );
            }
        }
        // 协程与模块走 native 主路径 (coroutine_lower 预重写 + BMF splice),
        // 不允许 fallback 回无法表达这些语义的 legacy C 文本路径.
        for f in [CppFeature::Coroutine, CppFeature::Module] {
            assert_eq!(f.level(), CppSupportLevel::EndToEnd);
            assert!(f.end_to_end_supported());
            assert!(f.native_supported());
        }
    }

    /// 能力矩阵的全量特性清单已上升为 `CppFeature::ALL_FEATURES` (非 test 可见),
    /// 供路径决策 / verbose 诊断相关的跨模块测试使用.
    #[allow(dead_code)]
    const ALL: &[CppFeature] = CppFeature::ALL_FEATURES;

    #[test]
    fn native_supported_includes_end_to_end_features() {
        // Stage 0: native_supported() 保留 lowering 覆盖面语义,
        // 因此它至少包含所有端到端支持的特性. 同样遍历全量矩阵.
        for f in ALL {
            assert!(!f.end_to_end_supported() || f.native_supported());
        }
    }
}
