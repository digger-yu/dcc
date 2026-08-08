//! dcc++ C++ 标准库模块
//!
//! 提供 dcc++ 的 C++ 标准库实现. 策略:
//!   - C 运行时 (libdccpp_std): 全部 std::* 容器 / IO / 异常的
//!     字节级实现, 编译成静态库 libdccpp_std.a
//!   - C++ 头文件 (src/cpp/stdlib/headers/): 用 dcc++ 模板类
//!     包装 C 运行时, 形成 std::vector<T> 等泛型容器.
//!     通过 dcc++ `#include` 加载.
//!
//! 模板支持:
//!   - M26 模板单态化引擎已完整, std::vector<T> 为真正的类模板
//!   - 实例化时 T 被替换为具体类型, sizeof(T) 自动计算
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
mod header_parse_tests;
#[cfg(test)]
mod tests;
