//! C++20 Modules 占位 (M18).
//!
//! 标准 C++ Modules (import / export / module) 的真实实现需要:
//!   1. 一个分离的 BMIP (Binary Module Interface) 文件格式
//!   2. 头文件输入到 BMIP 的预解析器
//!   3. 链接器对 BMIF 的合并支持
//!   4. ODR 检查
//!
//! dcc++ 当前没有 BMIP 实现, 但保留以下结构化占位, 以便后续
//! M19+ 在不改 API 的前提下替换实现:
//!
//! ## 已支持的语法层
//! - `module;` (global module fragment 起始, M9 占位)
//! - `export module name;` (module declaration, M9 占位)
//! - `export { ... }` (export block, M9 占位)
//! - `import name;` (import 头, M9 占位 - 当前等同于 #include)
//! - `export import name;` (re-export)
//!
//! ## 限制
//! - 无 BMIP 二进制缓存
//! - 跨 translation unit 不做 ODR 检查
//! - `import <header>` 行为等同 `#include <header>`
//! - 不支持 `module :private;` 私有片段 (M19+)
//! - 不支持 partitions (`module :part1`)
//!
//! ## 用法示例
//! ```cpp
//! module;
//! #include <cstdio>
//! export module mymod;
//! export int answer() { return 42; }
//! ```
//! 编译: `dcc++ -c mymod.cpp -o mymod.o` 然后在主程序
//! `import mymod;` 当前等价于 `#include "mymod.h"`.
//!
//! ## M19+ 计划
//! 1. 实现 BMF 序列化 (类 RIIR, 记录函数签名 + 类型布局)
//! 2. 链接时按 module 名合并 BMF
//! 3. `--emit-module-file=foo.bmf` 选项
//! 4. ODR 冲突检测

/// 模块声明缓存 (M19+ 真实化时填充)
#[derive(Debug, Clone, Default)]
pub struct ModuleInfo {
    pub name: String,
    pub exports: Vec<String>,
    pub imports: Vec<String>,
    pub bmf_path: Option<String>,
}

/// 已知模块名表 (进程内). M18: 仅注释, 不影响行为.
pub fn known_module(_name: &str) -> Option<ModuleInfo> { None }

/// 占位: 检查 `import X;` 是否应该展开为 #include. M18: 始终 true.
pub fn should_expand_to_include(_name: &str) -> bool { true }
