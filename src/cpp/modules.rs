//! C++20 Modules (M18 占位 -> M26.4 真实化).
//!
//! ## 已支持的语法层
//! - `module;` (global module fragment)
//! - `export module name;` (module declaration)
//! - `export module name:part;` (partition module declaration)
//! - `export { ... }` (export block)
//! - `import name;` (module import)
//! - `import :part;` (partition import, 并入本模块导出)
//! - `export import name;` (re-export)
//! - `module :private;` (private fragment, 终止导出收集)
//!
//! ## M26.4: BMIP 缓存 (BMF 文本接口)
//! - `--emit-module-file=foo.bmf`: 导出 TU 编译时写出模块接口 (DCC_BMF_V1)
//! - `import foo;`: 在搜索目录解析 `foo.bmf`, 注入 extern 声明 + 类布局
//! - `import :part;`: 解析 `<模块名>.<分区>.bmf` (`P`/`X` 记录), 分区导出并入
//! - 找不到 BMF 时回退为 `#include <foo>` (旧行为)
//! - M26.5: 跨 TU ODR 检查 (本模块导出 vs 导入模块导出, 同名不同定义报错)
//!
//! ## 剩余限制
//! - `import <header>` 行为等同 `#include <header>`

use crate::cpp::bmf::{BmfExport, BmfKind, BmfModule};
use crate::cpp::parser::CppDecl;

/// M26.4: 从声明树收集 `import` / `export module`, 在搜索目录中解析并
/// 加载 BMF 模块接口文件:
///   - `import foo;`    -> `foo.bmf`
///   - `import :part;`  -> `<当前模块名>.part.bmf`
///
/// 找不到时静默跳过 (lower 端回退为 include / 注释占位).
/// M27: 加载后执行 ODR 检查, 检测跨模块重复定义冲突.
pub fn load_imported_modules(
    decls: &[CppDecl],
    search_dirs: &[std::path::PathBuf],
) -> Vec<BmfModule> {
    let mut imports: Vec<String> = Vec::new();
    let mut module_name = String::new();
    for d in decls {
        match d {
            CppDecl::Import { path } => {
                if path.len() == 1 {
                    imports.push(path[0].clone());
                }
            },
            CppDecl::Module {
                name: Some(n),
                partition,
                ..
            } => {
                // 只有 export module 是当前 TU 的模块名; 但分区导入解析
                // 需要它, 这里记录最后一个有名字的 module 声明
                if module_name.is_empty() {
                    module_name = n.clone();
                }
                let _ = partition;
            },
            _ => {},
        }
    }
    let mut out: Vec<BmfModule> = Vec::new();
    for imp in imports {
        let fname = if let Some(part) = imp.strip_prefix(':') {
            // 分区导入: <模块名>.<分区>.bmf
            if module_name.is_empty() {
                continue;
            }
            format!("{}.{}.bmf", module_name, part)
        } else {
            format!("{}.bmf", imp)
        };
        for dir in search_dirs {
            let p = dir.join(&fname);
            if let Ok(text) = std::fs::read_to_string(&p) {
                if let Some(m) = BmfModule::parse(&text) {
                    out.push(m);
                }
                break;
            }
        }
    }
    // M27: ODR 检查 - 检测加载模块间的重复定义冲突
    if out.len() > 1 {
        let mut checker = OdrChecker::new();
        for m in &out {
            checker.register_module(&m.name, &m.exports);
        }
        if let Some(msg) = checker.check_and_format() {
            eprintln!("warning: {}", msg);
        }
    }
    out
}

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
            ..Default::default()
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
