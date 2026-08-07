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

use crate::cpp::bmf::{BmfExport, BmfKind, BmfModule};

/// 模块声明缓存 (M19+ 真实化时填充)
#[derive(Debug, Clone, Default)]
pub struct ModuleInfo {
    pub name: String,
    pub exports: Vec<String>,
    pub imports: Vec<String>,
    pub bmf_path: Option<String>,
}

impl ModuleInfo {
    /// 导出为 BMF 模块 (M26: 序列化接口文本)
    pub fn to_bmf(&self) -> BmfModule {
        BmfModule {
            name: self.name.clone(),
            exports: self
                .exports
                .iter()
                .map(|e| BmfExport {
                    kind: BmfKind::Function,
                    name: e.clone(),
                    signature: String::new(),
                    layout: None,
                })
                .collect(),
            imports: self.imports.clone(),
        }
    }

    /// 从 BMF 模块还原 (M26)
    pub fn from_bmf(b: &BmfModule) -> Self {
        ModuleInfo {
            name: b.name.clone(),
            exports: b.exports.iter().map(|e| e.name.clone()).collect(),
            imports: b.imports.clone(),
            bmf_path: None,
        }
    }
}

/// 已知模块名表 (进程内). M18: 仅注释, 不影响行为.
pub fn known_module(_name: &str) -> Option<ModuleInfo> {
    None
}

/// 占位: 检查 `import X;` 是否应该展开为 #include. M18: 始终 true.
pub fn should_expand_to_include(_name: &str) -> bool {
    true
}

/// M26: ODR (One Definition Rule) 检查器
/// 收集多个模块的导出实体, 检测重复定义冲突
#[derive(Debug, Default)]
pub struct OdrChecker {
    /// 实体名 -> (模块名, 签名/布局)
    definitions: std::collections::HashMap<String, Vec<(String, String)>>,
}

impl OdrChecker {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个模块的导出实体
    pub fn register_module(&mut self, module_name: &str, exports: &[BmfExport]) {
        for e in exports {
            let sig = match e.kind {
                BmfKind::Function => e.signature.clone(),
                BmfKind::Class => e.layout.clone().unwrap_or_default(),
                BmfKind::Variable => e.signature.clone(),
                BmfKind::Template => e.signature.clone(),
            };
            self.definitions
                .entry(e.name.clone())
                .or_default()
                .push((module_name.to_string(), sig));
        }
    }

    /// 检查 ODR 违规: 同名实体在不同模块中有不同定义
    /// 返回违规列表: (实体名, [(模块名, 签名), ...])
    pub fn check(&self) -> Vec<(String, Vec<(String, String)>)> {
        let mut violations = Vec::new();
        for (name, defs) in &self.definitions {
            if defs.len() > 1 {
                // 检查是否所有定义都相同
                let first_sig = &defs[0].1;
                let has_conflict = defs.iter().any(|(_, sig)| sig != first_sig);
                if has_conflict {
                    violations.push((name.clone(), defs.clone()));
                }
            }
        }
        violations
    }

    /// 检查并返回格式化的错误信息
    pub fn check_and_format(&self) -> Option<String> {
        let violations = self.check();
        if violations.is_empty() {
            return None;
        }
        let mut msg = String::from("ODR violations detected:\n");
        for (name, defs) in violations {
            msg.push_str(&format!("  entity '{}':\n", name));
            for (module, sig) in defs {
                msg.push_str(&format!("    in module '{}': {}\n", module, sig));
            }
        }
        Some(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpp::bmf::{BmfExport, BmfKind};

    #[test]
    fn odr_checker_no_violations() {
        let mut checker = OdrChecker::new();
        // 模块 A 导出 foo(int)
        let exports_a = vec![BmfExport {
            kind: BmfKind::Function,
            name: "foo".to_string(),
            signature: "int(int)".to_string(),
            layout: None,
        }];
        checker.register_module("A", &exports_a);
        // 模块 B 导出 bar(double) - 不同名字, 无冲突
        let exports_b = vec![BmfExport {
            kind: BmfKind::Function,
            name: "bar".to_string(),
            signature: "double(double)".to_string(),
            layout: None,
        }];
        checker.register_module("B", &exports_b);
        assert!(checker.check().is_empty());
        assert!(checker.check_and_format().is_none());
    }

    #[test]
    fn odr_checker_same_definition_ok() {
        let mut checker = OdrChecker::new();
        // 模块 A 和 B 都导出相同签名的 foo(int)
        let exports = vec![BmfExport {
            kind: BmfKind::Function,
            name: "foo".to_string(),
            signature: "int(int)".to_string(),
            layout: None,
        }];
        checker.register_module("A", &exports);
        checker.register_module("B", &exports);
        // 相同定义不算 ODR 违规
        assert!(checker.check().is_empty());
    }

    #[test]
    fn odr_checker_conflict_detected() {
        let mut checker = OdrChecker::new();
        // 模块 A 导出 foo(int)
        let exports_a = vec![BmfExport {
            kind: BmfKind::Function,
            name: "foo".to_string(),
            signature: "int(int)".to_string(),
            layout: None,
        }];
        checker.register_module("A", &exports_a);
        // 模块 B 导出 foo(double) - 同名但不同签名, ODR 违规
        let exports_b = vec![BmfExport {
            kind: BmfKind::Function,
            name: "foo".to_string(),
            signature: "int(double)".to_string(),
            layout: None,
        }];
        checker.register_module("B", &exports_b);
        let violations = checker.check();
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].0, "foo");
        let msg = checker.check_and_format().unwrap();
        assert!(msg.contains("ODR violations"));
        assert!(msg.contains("foo"));
        assert!(msg.contains("module 'A'"));
        assert!(msg.contains("module 'B'"));
    }
}
