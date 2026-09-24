//! C++ 前端 (M7+ 最小可编译子集)
//!
//! 目标: 实现 GCC 编译 C++ 子集所必需的最小特性, 满足
//! GCC bootstrap 阶段 1 (cc1plus) 编译时不会立刻触雷。
//!
//! 设计选择: C++ 源码经 cpp 前端解析后, 降级 (lower) 到 dcc 现有
//! 的 C IR / C 风格声明, 复用 codegen + link 链路。
//! 不另起一套 IR, 避免 4 倍维护负担。
//!
//! M7 范围:
//!   - C++ token 扩展: `class`, `public`, `private`, `protected`,
//!     `this`, `new`, `delete`, `::`, `operator`
//!   - class 声明: 成员变量 + 成员函数
//!   - this->member 访问 (隐式 this 形参)
//!   - :: 作用域解析 (命名空间 / 类作用域)
//!   - T& 引用类型 (降级为指针)
//!   - 构造 / 析构函数 (Itanium 名字修饰 _ZN* C1 / D1)
//!   - new T / delete p (调用 ::operator new / ::operator delete)
//!   - Itanium C++ ABI 名字修饰 (mangle.rs)
//!
//! M8: 模板 / 异常 / 虚函数 / vtable / 命名空间 / operator 重载
//! M9: concept 占位, module 占位, lambda, 多继承, lambda
//! M10+: 完整 SFINAE / 偏特化 / concept 求值 / 协程 / module 分区
//!      / operator-> 链 / 跨虚继承 dynamic_cast / 构造委托 /
//!      成员函数指针 / __cxa_guard_ / initializer_list
//!
//! 文件:
//!   - lexer.rs:  C++ token 流 (基于 C tokens, 扩展关键字)
//!   - parser.rs: 类 / 成员函数 / this / :: / 引用 解析
//!   - mangle.rs: Itanium C++ ABI 名字修饰
//!   - lower.rs:  C++ AST → dcc C 风格声明 / 函数
//!   - feature_degrade.rs: 高级特性降级 (SFINAE / concept / 偏特化 / 协程 / RTTI 等)
//!   - template.rs: 模板完整单态化 (M26: 类/函数模板实例化 + 缓存)
//!   - coroutine.rs: 协程真状态机 (M26: Duff's device 挂起/恢复)
//!   - bmf.rs:      C++20 modules BMF 序列化 (M26: 模块接口文本格式)
//!   - gcc_compat.rs: GCC ABI 兼容运行时 (M26: __cxa_guard / typeinfo / dynamic_cast)
//!   - std/:      M11 C++ 标准库 (头 + C 运行时)
//!   - driver.rs: 顶层入口, 串 lexer→parser→lower→dcc driver
//!
//! 限制: C++ 前端在 M7+ 阶段只支持 dcc 既有的 C 子集能直接
//! 表达的特性 (struct-as-class, 成员函数, this 隐式形参化,
//! 引用→指针退化). GCC 真实 bootstrap 会因模板/异常/虚函数
//! 失败, 已在 docs/missing_features.md 与 docs/gcc_build_guide.md
//! 中列明.
#![allow(dead_code, unused_imports)]

pub mod abi_layout;
pub mod adl;
pub mod bmf;
pub mod coroutine;
pub mod cpp_const_eval;
pub mod driver;
pub mod feature_degrade;
pub mod feature_matrix;
pub mod gcc_compat;
pub mod hir;
pub mod legacy;
pub mod lexer;
pub mod lower;
pub mod lower_ir;
pub mod mangle;
pub mod modules;
pub mod native;
pub mod parser;
pub mod runtime;
pub mod sema;
pub mod stdlib;
pub mod template;
pub mod template_depth;

use crate::diagnostics::Diagnostic;
use std::path::PathBuf;

/// C++ 编译选项 (M7 最小)
#[derive(Debug, Clone)]
pub struct CppOptions {
    /// 输入文件 (.cpp / .cc / .cxx)
    pub input: PathBuf,
    /// 输出文件 (-o)
    pub output: PathBuf,
    /// 头文件搜索路径
    pub include_paths: Vec<PathBuf>,
    /// 系统头文件搜索路径
    pub system_paths: Vec<PathBuf>,
    /// -D 宏定义
    pub defines: Vec<(String, Option<String>)>,
    /// 仅做语法检查
    pub syntax_only: bool,
    /// 优化级别 0..3
    pub opt_level: u8,
}

/// C++ 编译错误
#[derive(Debug)]
pub enum CppError {
    Diagnostics(Vec<Diagnostic>),
    Io(std::io::Error),
}

impl std::fmt::Display for CppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CppError::Diagnostics(ds) => {
                for d in ds {
                    writeln!(f, "{}", d)?;
                }
                Ok(())
            },
            CppError::Io(e) => write!(f, "I/O error: {}", e),
        }
    }
}

impl From<std::io::Error> for CppError {
    fn from(e: std::io::Error) -> Self {
        CppError::Io(e)
    }
}
