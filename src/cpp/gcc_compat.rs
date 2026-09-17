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

/// SJLJ 风格异常处理运行时 (Stage 9)
///
/// 实现基于 setjmp/longjmp 的异常栈,用于 native path 的 throw/catch.
/// 与 libstdc++ 的 __cxa_allocate_exception / __cxa_free_exception 配合使用.
///
/// 结构:
/// - __dcc_eh_context_t: 每个 try 块的异常上下文 (含 jmp_buf + 异常指针)
/// - 线程局部异常栈: 链表结构, push/pop 管理 (使用 __thread)
///
/// 注意: 这是 SJLJ (setjmp/longjmp) 风格, 不是零成本异常.
/// 每个 try 块入口有开销, 但 throw/catch 能正确工作.
pub fn sjlj_eh_runtime() -> String {
    String::from(
        "/* dcc GCC ABI: SJLJ exception handling runtime (Stage 9) */\n\
         #include <setjmp.h>\n\
         #include <stdlib.h>\n\
         #include <string.h>\n\
         \n\
         /* 自包含异常内存分配/释放: native path 不链接 libstdc++, 因此\n\
            __cxa_allocate_exception/__cxa_free_exception 用 malloc/free 提供.\n\
            简化 SJLJ 方案只把异常对象当裸内存, 无 Itanium 头部偏移. */\n\
         static void* __cxa_allocate_exception(size_t n) {\n\
         \x20   return malloc(n ? n : 1);\n\
         }\n\
         static void __cxa_free_exception(void* p) {\n\
         \x20   free(p);\n\
         }\n\
         \n\
         /* 异常上下文: 每个 try 块一个 */\n\
         typedef struct __dcc_eh_context {\n\
         \x20   jmp_buf jmpbuf;\n\
         \x20   void* exception_ptr;\n\
         \x20   void* type_info;\n\
         \x20   struct __dcc_eh_context* next;\n\
         \x20   int caught; /* 是否已被 catch 处理 */\n\
         } __dcc_eh_context_t;\n\
         \n\
         /* 线程局部异常栈顶指针 (thread-local storage) */\n\
         static __thread __dcc_eh_context_t* __dcc_eh_stack_top = 0;\n\
         \n\
         /* 当前正在处理的异常 (用于 rethrow) */\n\
         static __thread void* __dcc_eh_current_exn = 0;\n\
         static __thread void* __dcc_eh_current_ti = 0;\n\
         \n\
         /* 推入异常上下文 (进入 try 块时调用) */\n\
         static inline void __dcc_eh_push_context(void* ctx) {\n\
         \x20   __dcc_eh_context_t* c = (__dcc_eh_context_t*)ctx;\n\
         \x20   c->exception_ptr = 0;\n\
         \x20   c->type_info = 0;\n\
         \x20   c->caught = 0;\n\
         \x20   c->next = __dcc_eh_stack_top;\n\
         \x20   __dcc_eh_stack_top = c;\n\
         }\n\
         \n\
         /* 弹出异常上下文 (正常退出 try 块时调用) */\n\
         static inline void __dcc_eh_pop_context(void) {\n\
         \x20   if (__dcc_eh_stack_top) {\n\
         \x20       __dcc_eh_stack_top = __dcc_eh_stack_top->next;\n\
         \x20   }\n\
         }\n\
         \n\
         /* 设置 catch 点 (返回 0 表示正常进入, 非 0 表示异常抛出) */\n\
         static inline int __dcc_eh_setjmp(void* ctx) {\n\
         \x20   __dcc_eh_context_t* c = (__dcc_eh_context_t*)ctx;\n\
         \x20   return setjmp(c->jmpbuf);\n\
         }\n\
         \n\
         /* 获取当前异常对象指针 */\n\
         static inline void* __dcc_eh_get_exception(void* ctx) {\n\
         \x20   __dcc_eh_context_t* c = (__dcc_eh_context_t*)ctx;\n\
         \x20   return c->exception_ptr;\n\
         }\n\
         \n\
         /* 类型匹配检查 (简化: 比较 typeinfo 指针) */\n\
         static inline int __dcc_eh_type_match(void* exn, void* ti) {\n\
         \x20   (void)exn;\n\
         \x20   /* 简化: ti == 0 视为 catch-all (总是匹配); 类类型按指针等比较 */\n\
         \x20   if (ti == 0) return 1;\n\
         \x20   if (__dcc_eh_current_ti == 0) return 0;\n\
         \x20   return __dcc_eh_current_ti == ti;\n\
         }\n\
         \n\
         /* 抛出异常 (SJLJ 风格: 直接 longjmp 到栈顶) */\n\
         static inline void __dcc_eh_throw(void* exn, void* ti) {\n\
         \x20   __dcc_eh_current_exn = exn;\n\
         \x20   __dcc_eh_current_ti = ti;\n\
         \x20   if (__dcc_eh_stack_top) {\n\
         \x20       __dcc_eh_stack_top->exception_ptr = exn;\n\
         \x20       __dcc_eh_stack_top->type_info = ti;\n\
         \x20       longjmp(__dcc_eh_stack_top->jmpbuf, 1);\n\
         \x20   }\n\
         \x20   /* 没有 catch handler: 调用 terminate (简化: abort) */\n\
         \x20   abort();\n\
         }\n\
         \n\
         /* 重新抛出当前异常 */\n\
         static inline void __dcc_eh_rethrow(void) {\n\
         \x20   void* exn = __dcc_eh_current_exn;\n\
         \x20   void* ti = __dcc_eh_current_ti;\n\
         \x20   /* 弹出当前 context (catch 块的 context) */\n\
         \x20   if (__dcc_eh_stack_top) {\n\
         \x20       __dcc_eh_stack_top = __dcc_eh_stack_top->next;\n\
         \x20   }\n\
         \x20   if (__dcc_eh_stack_top && exn) {\n\
         \x20       __dcc_eh_stack_top->exception_ptr = exn;\n\
         \x20       __dcc_eh_stack_top->type_info = ti;\n\
         \x20       longjmp(__dcc_eh_stack_top->jmpbuf, 1);\n\
         \x20   }\n\
         \x20   abort();\n\
         }\n\
         \n\
         /* 结束 catch 处理 */\n\
         static inline void __dcc_eh_end_catch(void* ctx) {\n\
         \x20   __dcc_eh_context_t* c = (__dcc_eh_context_t*)ctx;\n\
         \x20   c->caught = 1;\n\
         \x20   /* 弹出当前 context */\n\
         \x20   if (__dcc_eh_stack_top == c) {\n\
         \x20       __dcc_eh_stack_top = c->next;\n\
         \x20   }\n\
         \x20   /* 释放异常内存 (使用 __cxa_free_exception) */\n\
         \x20   if (c->exception_ptr) {\n\
         \x20       __cxa_free_exception(c->exception_ptr);\n\
         \x20       c->exception_ptr = 0;\n\
         \x20   }\n\
         \x20   __dcc_eh_current_exn = 0;\n\
         \x20   __dcc_eh_current_ti = 0;\n\
         }\n",
    )
}

/// 完整运行时前导: 追加到 lower 输出前部 (幂等, 可重复安全)
pub fn runtime_prologue() -> String {
    let mut s = cxa_guard_runtime();
    s.push_str(&typeinfo_struct_decl());
    s.push_str(&dynamic_cast_runtime());
    s.push_str(&initializer_list_layout());
    s.push_str(&member_ptr_helpers());
    s.push_str(&sjlj_eh_runtime());
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
        assert!(s.contains("__dcc_eh_throw"), "{}", s);
        assert!(s.contains("__dcc_eh_push_context"), "{}", s);
    }

    #[test]
    fn sjlj_eh_runtime_has_all_functions() {
        let s = sjlj_eh_runtime();
        assert!(s.contains("__dcc_eh_context_t"), "{}", s);
        assert!(s.contains("__dcc_eh_push_context"), "{}", s);
        assert!(s.contains("__dcc_eh_pop_context"), "{}", s);
        assert!(s.contains("__dcc_eh_setjmp"), "{}", s);
        assert!(s.contains("__dcc_eh_get_exception"), "{}", s);
        assert!(s.contains("__dcc_eh_type_match"), "{}", s);
        assert!(s.contains("__dcc_eh_throw"), "{}", s);
        assert!(s.contains("__dcc_eh_rethrow"), "{}", s);
        assert!(s.contains("__dcc_eh_end_catch"), "{}", s);
    }

    #[test]
    fn sjlj_eh_runtime_uses_thread_local_storage() {
        // 修复: 异常栈与当前异常状态必须真正线程局部,
        // 否则多线程下会跨栈 longjmp 与 catch 误匹配
        let s = sjlj_eh_runtime();
        assert!(s.contains("__thread"), "expected __thread TLS, got: {}", s);
        assert!(s.contains("__dcc_eh_stack_top"), "{}", s);
        assert!(s.contains("__dcc_eh_current_exn"), "{}", s);
        assert!(s.contains("__dcc_eh_current_ti"), "{}", s);
    }
}
