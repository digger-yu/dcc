//! dcc++ C++ 标准库模块
//!
//! 提供 dcc++ 的 C++ 标准库实现. 策略:
//!   - C 运行时 (libdccpp_std): 全部 std::* 容器 / IO / 异常的
//!     字节级实现, 编译成静态库 libdccpp_std.a
//!   - C++ 头文件 (src/cpp/std/headers/): 用 dcc++ 可表达的非模板
//!     C++ 语法包装 C 运行时, 形成 std::string / std::vector 等
//!     类. 通过 dcc++ `#include` 加载.
//!
//! 已知限制 (源自 dcc++ 模板支持):
//!   - dcc++ 暂不支持模板实例化语法 `T<U>` (parse_mangle_type
//!     不识别 `<`), 故 std::vector<T> / std::map<K,V> 不能直接
//!     `vector<int>` 形式使用. 当前采用"单一非模板类"策略:
//!       * 元素大小由用户传入
//!       * 析构 / 比较通过函数指针回调
//!     后续 M11+ 若支持 SFINAE / 完整模板, 可提供真正泛型版本.
//!
//! 文件:
//!   - runtime/dccpp_runtime.c: C 运行时源 (gcc/clang 编译)
//!   - headers/cstddef cstdlib cstring cstdio: C 标准库转发
//!   - headers/string vector iostream memory exception stdexcept
//!     typeinfo initializer_list utility algorithm map functional:
//!     dcc++ C++ 头
//!
//! 限制: 与 dcc 现有 lower.rs 的 __cxa_throw/__cxa_allocate_exception
//! ABI 集成, 头文件用 `extern "C"` 引用 C 运行时符号.

#![allow(dead_code, unused_imports)]

/// C 运行时源文件路径 (相对于 crate root)
pub const RUNTIME_C_PATH: &str = "src/cpp/stdlib/runtime/dccpp_runtime.c";

/// C 头文件搜索路径
pub const HEADER_DIR: &str = "src/cpp/stdlib/headers";

/// 已注册的标准库头列表
pub const STD_HEADERS: &[&str] = &[
    "cstddef",
    "cstdlib",
    "cstring",
    "cstdio",
    "string",
    "vector",
    "iostream",
    "memory",
    "exception",
    "stdexcept",
    "typeinfo",
    "initializer_list",
    "utility",
    "algorithm",
    "map",
    "functional",
    // M18: 新增
    "chrono",
    "thread",
    "mutex",
    "condition_variable",
    "filesystem",
    "expected",
    "flat_map",
    "regex",
    "ranges",
];

/// 是否启用 std 库 (M11+ 标记)
pub const STDLIB_ENABLED: bool = true;

/// 描述 std:: 容器接口 (用于 dcc++ lowering 时插入 #include 行)
pub fn emit_stdlib_includes() -> String {
    let mut s = String::new();
    s.push_str("#include <cstddef>\n");
    s.push_str("#include <cstdlib>\n");
    s.push_str("#include <cstring>\n");
    s.push_str("#include <cstdio>\n");
    s.push_str("#include <string>\n");
    s.push_str("#include <vector>\n");
    s.push_str("#include <iostream>\n");
    s.push_str("#include <memory>\n");
    s.push_str("#include <exception>\n");
    s.push_str("#include <stdexcept>\n");
    s.push_str("#include <typeinfo>\n");
    s.push_str("#include <initializer_list>\n");
    s.push_str("#include <utility>\n");
    s.push_str("#include <algorithm>\n");
    s.push_str("#include <map>\n");
    s.push_str("#include <functional>\n");
    // M18: 新增
    s.push_str("#include <chrono>\n");
    s.push_str("#include <thread>\n");
    s.push_str("#include <mutex>\n");
    s.push_str("#include <condition_variable>\n");
    s.push_str("#include <filesystem>\n");
    s.push_str("#include <expected>\n");
    s.push_str("#include <flat_map>\n");
    s.push_str("#include <regex>\n");
    s.push_str("#include <ranges>\n");
    s
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod header_parse_tests;
