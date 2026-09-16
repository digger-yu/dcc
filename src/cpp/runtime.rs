//! C++ runtime symbol glue.
//!
//! 管理 C++ ABI runtime symbol:
//! - `__cxa_*` (cxa_atexit, cxa_guard_*, cxa_pure_virtual, etc.)
//! - `operator new / delete`
//! - `type_info` / `vtable` 符号
//! - `__gxx_personality_v0`
//!
//! 与 legacy 的 gcc_compat.rs / runtime prologue 不同:
//! - 只声明确实需要的 symbol，不输出整段 typedef
//! - native path 优先使用系统 libstdc++ 符号
//! - freestanding 模式才用 dcc 自有实现
//!
//! 当前阶段: 骨架 + 枚举定义。

/// C++ runtime 所需的外部符号
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CxxRuntimeSymbol {
    // --- 基础 runtime ---
    /// `__cxa_pure_virtual`
    PureVirtual,
    /// `__cxa_atexit`
    AtExit,

    // --- 静态初始化 guard ---
    /// `__cxa_guard_acquire`
    GuardAcquire,
    /// `__cxa_guard_release`
    GuardRelease,
    /// `__cxa_guard_abort`
    GuardAbort,

    // --- operator new / delete ---
    /// `operator new(size_t)`
    New,
    /// `operator delete(void*)`
    Delete,
    /// `operator new[](size_t)`
    NewArray,
    /// `operator delete[](void*)`
    DeleteArray,
    /// placement new 不需要 runtime 调用（只是 cast）

    // --- 异常 ---
    /// `__cxa_allocate_exception`
    AllocException,
    /// `__cxa_free_exception`
    FreeException,
    /// `__cxa_throw`
    Throw,
    /// `__cxa_rethrow`
    Rethrow,
    /// `__cxa_begin_catch`
    BeginCatch,
    /// `__cxa_end_catch`
    EndCatch,
    /// `__gxx_personality_v0`
    PersonalityV0,

    // --- RTTI ---
    /// `__cxa_bad_cast`
    BadCast,
    /// `__cxa_bad_typeid`
    BadTypeid,
    /// `__dynamic_cast`
    DynamicCast,
}

impl CxxRuntimeSymbol {
    /// 获取符号的 C 名称
    pub fn name(&self) -> &'static str {
        match self {
            Self::PureVirtual => "__cxa_pure_virtual",
            Self::AtExit => "__cxa_atexit",
            Self::GuardAcquire => "__cxa_guard_acquire",
            Self::GuardRelease => "__cxa_guard_release",
            Self::GuardAbort => "__cxa_guard_abort",
            Self::New => "_Znwm",           // operator new(unsigned long)
            Self::Delete => "_ZdlPv",       // operator delete(void*)
            Self::NewArray => "_Znam",      // operator new[](unsigned long)
            Self::DeleteArray => "_ZdaPv",  // operator delete[](void*)
            Self::AllocException => "__cxa_allocate_exception",
            Self::FreeException => "__cxa_free_exception",
            Self::Throw => "__cxa_throw",
            Self::Rethrow => "__cxa_rethrow",
            Self::BeginCatch => "__cxa_begin_catch",
            Self::EndCatch => "__cxa_end_catch",
            Self::PersonalityV0 => "__gxx_personality_v0",
            Self::BadCast => "__cxa_bad_cast",
            Self::BadTypeid => "__cxa_bad_typeid",
            Self::DynamicCast => "__dynamic_cast",
        }
    }

    /// 是否是异常相关符号
    pub fn is_exception(&self) -> bool {
        matches!(
            self,
            Self::AllocException
                | Self::FreeException
                | Self::Throw
                | Self::Rethrow
                | Self::BeginCatch
                | Self::EndCatch
                | Self::PersonalityV0
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_names_are_non_empty() {
        let all = [
            CxxRuntimeSymbol::PureVirtual,
            CxxRuntimeSymbol::AtExit,
            CxxRuntimeSymbol::GuardAcquire,
            CxxRuntimeSymbol::GuardRelease,
            CxxRuntimeSymbol::GuardAbort,
            CxxRuntimeSymbol::New,
            CxxRuntimeSymbol::Delete,
            CxxRuntimeSymbol::NewArray,
            CxxRuntimeSymbol::DeleteArray,
            CxxRuntimeSymbol::AllocException,
            CxxRuntimeSymbol::FreeException,
            CxxRuntimeSymbol::Throw,
            CxxRuntimeSymbol::Rethrow,
            CxxRuntimeSymbol::BeginCatch,
            CxxRuntimeSymbol::EndCatch,
            CxxRuntimeSymbol::PersonalityV0,
            CxxRuntimeSymbol::BadCast,
            CxxRuntimeSymbol::BadTypeid,
            CxxRuntimeSymbol::DynamicCast,
        ];
        for s in &all {
            assert!(!s.name().is_empty());
        }
    }

    #[test]
    fn exception_symbols() {
        assert!(CxxRuntimeSymbol::Throw.is_exception());
        assert!(CxxRuntimeSymbol::BeginCatch.is_exception());
        assert!(!CxxRuntimeSymbol::New.is_exception());
        assert!(!CxxRuntimeSymbol::PureVirtual.is_exception());
    }
}
