//! C++ feature matrix: 枚举 C++ 前端支持的能力
//!
//! 用于:
//! - capability tracing: 记录某个 TU 走 native 还是 legacy 路径
//! - auto mode: 判断是否可以走 native path
//! - smoke test: 标记迁移保护用例
//!
//! 能力按方案文档的 Stage 顺序排列，逐步从 legacy 迁移到 native。

/// C++ 特性能力标识。每个能力对应方案中一个迁移里程碑。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CppFeature {
    // --- Stage 3: 普通 class / 函数 ---
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
}

impl CppFeature {
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
        }
    }

    /// 当前 native path 已支持的特性集合
    /// 随阶段推进逐步扩展。
    pub fn native_supported(&self) -> bool {
        // Stage 9: 异常 (throw/catch) 已支持（SJLJ 风格）。
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
        )
    }
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[derive(Default)]
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
}
