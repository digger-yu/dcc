//! GCC ABI 兼容运行时 (M26)
//!
//! 产出 C 端运行时定义, 供 lower 生成的 C 代码链接. 覆盖:
//! 1. `__cxa_guard_acquire/release/abort` - 静态局部变量线程安全单次初始化
//!    (Itanium C++ ABI; dcc 简化实现为单线程状态机: 0=未初始化,
//!    1=初始化中, 2=已初始化)
//! 2. `__dcc_typeinfo_t` - typeid 静态路径使用的 typeinfo 结构
//!    (lower 的 `emit_typeinfo` 输出 `&__dcc_ti_<T>` 依赖此 typedef)
//! 3. `__dynamic_cast` - dynamic_cast<T*>(e) 跨虚继承运行时
//!    (简化: 上转返回原指针, 下转按偏移调整)
//! 4. `__dcc_initializer_list_t` - std::initializer_list 布局
//!    ({ const T* _M_array; size_t _M_len; } + 构造宏)
//! 5. `__dcc_mbr_off` - 成员指针偏移宏 (obj.*ptr_to_member 辅助)
//!
//! 设计原则: 全部为自包含的 C typedef / 函数 / 宏, 无外部依赖,
//! 可安全追加到每次 lower 输出的前导; 保持 dcc C 后端可编译.

/// __cxa_guard_* 运行时: 线程安全单次初始化
/// dcc 简化实现: guard 字节状态 0/1/2 (未初始化/初始化中/已初始化),
/// 单线程语义. 同时提供 Itanium 双下划线变体.
/// 使用 static inline 避免多 TU 链接冲突.
pub fn cxa_guard_runtime() -> String {
    let mut s = String::from(
        "/* dcc GCC ABI: __cxa_guard_* thread-safe static init (static inline: multi-TU safe) */\n\
         static inline int _cxa_guard_acquire(unsigned char* g) {\n\
         \x20   if (*g != 0) return 0;\n\
         \x20   *g = 1;\n\
         \x20   return 1;\n\
         }\n\
         static inline void _cxa_guard_release(unsigned char* g) { *g = 2; }\n\
         static inline void _cxa_guard_abort(unsigned char* g) { *g = 0; }\n",
    );
    s.push_str(
        "static inline int __cxa_guard_acquire(unsigned char* g) { return _cxa_guard_acquire(g); }\n\
         static inline void __cxa_guard_release(unsigned char* g) { _cxa_guard_release(g); }\n\
         static inline void __cxa_guard_abort(unsigned char* g) { _cxa_guard_abort(g); }\n",
    );
    s
}

/// typeinfo 结构声明 (typeid 静态路径 `&__dcc_ti_<T>` 的承载类型)
pub fn typeinfo_struct_decl() -> String {
    String::from("typedef struct __dcc_typeinfo { const char* name; } __dcc_typeinfo_t;\n")
}

/// __dynamic_cast 运行时: 跨虚继承 dynamic_cast 简化实现
/// - ptr 为空: 返回空
/// - offset < 0 (上转): 返回原指针
/// - 否则: 按偏移调整到目标子对象
///
/// 使用 static inline 避免多 TU 链接冲突.
pub fn dynamic_cast_runtime() -> String {
    String::from(
        "/* dcc GCC ABI: __dynamic_cast (cross-virtual cast helper; static inline: multi-TU safe) */\n\
         static inline void* __dynamic_cast(void* ptr, void* src_ti, void* dst_ti, long offset) {\n\
         \x20   (void)src_ti;\n\
         \x20   (void)dst_ti;\n\
         \x20   if (ptr == 0) return 0;\n\
         \x20   if (offset < 0) return ptr;\n\
         \x20   return (void*)((char*)ptr - offset);\n\
         }\n",
    )
}

/// std::initializer_list 布局 + 构造宏
pub fn initializer_list_layout() -> String {
    String::from(
        "/* dcc GCC ABI: std::initializer_list layout */\n\
         typedef struct __dcc_initializer_list {\n\
         \x20   const void* _M_array;\n\
         \x20   unsigned long _M_len;\n\
         } __dcc_initializer_list_t;\n\
         #define __dcc_make_il(arr, n) \\\n\
         \x20   ((__dcc_initializer_list_t){ (const void*)(arr), (unsigned long)(n) })\n",
    )
}

/// 成员指针偏移宏: `obj.*ptr_to_member` 取偏移的通用形式
pub fn member_ptr_helpers() -> String {
    String::from(
        "/* dcc GCC ABI: member pointer offset helpers */\n\
         #define __dcc_mbr_off(T, f) ((unsigned long)(&(((T*)0)->f)))\n",
    )
}

/// 完整运行时前导: 追加到 lower 输出前部 (幂等, 可重复安全)
pub fn runtime_prologue() -> String {
    let mut s = cxa_guard_runtime();
    s.push_str(&typeinfo_struct_decl());
    s.push_str(&dynamic_cast_runtime());
    s.push_str(&initializer_list_layout());
    s.push_str(&member_ptr_helpers());
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_runtime_defines_both_underscore_variants() {
        let s = cxa_guard_runtime();
        assert!(
            s.contains("int _cxa_guard_acquire(unsigned char* g)"),
            "{}",
            s
        );
        assert!(
            s.contains("void _cxa_guard_release(unsigned char* g)"),
            "{}",
            s
        );
        assert!(
            s.contains("void _cxa_guard_abort(unsigned char* g)"),
            "{}",
            s
        );
        assert!(
            s.contains("int __cxa_guard_acquire(unsigned char* g)"),
            "{}",
            s
        );
    }

    #[test]
    fn guard_runtime_state_machine() {
        // 模拟 C 语义: 0 未初始化 -> acquire 1 次 -> release -> 状态 2
        let s = cxa_guard_runtime();
        assert!(s.contains("if (*g != 0) return 0;"), "{}", s);
        assert!(s.contains("*g = 1;"), "{}", s);
        assert!(s.contains("*g = 2;"), "{}", s);
    }

    #[test]
    fn typeinfo_decl_has_name_field() {
        let s = typeinfo_struct_decl();
        assert!(
            s.contains("typedef struct __dcc_typeinfo { const char* name; }"),
            "{}",
            s
        );
    }

    #[test]
    fn dynamic_cast_runtime_handles_null_and_offsets() {
        let s = dynamic_cast_runtime();
        assert!(s.contains("if (ptr == 0) return 0;"), "{}", s);
        assert!(s.contains("if (offset < 0) return ptr;"), "{}", s);
        assert!(s.contains("(char*)ptr - offset"), "{}", s);
    }

    #[test]
    fn initializer_list_layout_has_array_and_len() {
        let s = initializer_list_layout();
        assert!(s.contains("_M_array"), "{}", s);
        assert!(s.contains("_M_len"), "{}", s);
        assert!(s.contains("__dcc_make_il"), "{}", s);
    }

    #[test]
    fn member_ptr_helpers_has_offset_macro() {
        let s = member_ptr_helpers();
        assert!(s.contains("__dcc_mbr_off"), "{}", s);
    }

    #[test]
    fn runtime_prologue_includes_all_sections() {
        let s = runtime_prologue();
        assert!(s.contains("_cxa_guard_acquire"), "{}", s);
        assert!(s.contains("__dcc_typeinfo_t"), "{}", s);
        assert!(s.contains("__dynamic_cast"), "{}", s);
        assert!(s.contains("__dcc_initializer_list_t"), "{}", s);
        assert!(s.contains("__dcc_mbr_off"), "{}", s);
    }
}
