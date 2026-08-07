//! C++20 modules BMF 序列化 (M26)
//!
//! BMF (Binary Module Format) 是 dcc 自定义的模块接口文件格式:
//! 记录模块名 + export 声明 (函数签名 / 类布局 / 变量) + import 列表,
//! 供跨 translation unit 合并模块接口 (M19+ 计划).
//!
//! 实现: 零依赖文本格式, 前缀 magic `DCC_BMF_V1`, 每行一条记录:
//! ```text
//! DCC_BMF_V1
//! M <module-name>
//! I <import-name>
//! E F <fn-name> <signature>
//! E C <class-name> <layout>
//! E V <var-name> <type>
//! ```
//! - `M`: 模块名
//! - `I`: import 依赖
//! - `E F`: export 函数 (signature 形如 `int(int, char*)`)
//! - `E C`: export 类 (layout 形如 `x:int, y:double`)
//! - `E V`: export 变量
//!
//! 设计原则: 纯字符串序列化, 不依赖 lower / parser 上下文;
//! `serialize` / `parse` 互为逆操作 (round-trip).

/// BMF 文件 magic 头
pub const BMF_MAGIC: &str = "DCC_BMF_V1";

/// export 条目类型
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BmfKind {
    Function,
    Class,
    Variable,
    /// M26: export template 模板导出
    Template,
}

/// 单条 export 记录
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BmfExport {
    pub kind: BmfKind,
    /// 导出名 (mangled 或源名)
    pub name: String,
    /// 函数: 签名文本 `ret(p1,p2)`; 变量: 类型文本; 类: 空
    pub signature: String,
    /// 类: 成员布局描述 (`name:type, ...`); 其他: None
    pub layout: Option<String>,
}

/// 模块接口 (BMF 的进程内表示)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BmfModule {
    pub name: String,
    pub exports: Vec<BmfExport>,
    pub imports: Vec<String>,
}

impl BmfModule {
    /// 序列化为 BMF 文本
    pub fn serialize(&self) -> String {
        let mut out = format!("{}\n", BMF_MAGIC);
        out.push_str(&format!("M {}\n", self.name));
        for i in &self.imports {
            out.push_str(&format!("I {}\n", i));
        }
        for e in &self.exports {
            match e.kind {
                BmfKind::Function => {
                    out.push_str(&format!("E F {} {}\n", e.name, e.signature));
                }
                BmfKind::Class => {
                    out.push_str(&format!(
                        "E C {} {}\n",
                        e.name,
                        e.layout.as_deref().unwrap_or("")
                    ));
                }
                BmfKind::Variable => {
                    out.push_str(&format!("E V {} {}\n", e.name, e.signature));
                }
                BmfKind::Template => {
                    // M26: 模板导出格式 E T <name> <params>
                    out.push_str(&format!("E T {} {}\n", e.name, e.signature));
                }
            }
        }
        out
    }

    /// 解析 BMF 文本; 格式不合法返回 None
    pub fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != BMF_MAGIC {
            return None;
        }
        let mut m = BmfModule::default();
        for line in lines {
            if line.trim().is_empty() {
                continue;
            }
            let mut parts = line.splitn(4, ' ');
            let tag = parts.next()?;
            match tag {
                "M" => m.name = parts.next()?.trim().to_string(),
                "I" => m.imports.push(parts.next()?.trim().to_string()),
                "E" => {
                    let kind = parts.next()?;
                    let name = parts.next()?;
                    let rest = parts.next().unwrap_or("").trim();
                    match kind {
                        "F" => m.exports.push(BmfExport {
                            kind: BmfKind::Function,
                            name: name.to_string(),
                            signature: rest.to_string(),
                            layout: None,
                        }),
                        "C" => m.exports.push(BmfExport {
                            kind: BmfKind::Class,
                            name: name.to_string(),
                            signature: String::new(),
                            layout: if rest.is_empty() { None } else { Some(rest.to_string()) },
                        }),
                        "V" => m.exports.push(BmfExport {
                            kind: BmfKind::Variable,
                            name: name.to_string(),
                            signature: rest.to_string(),
                            layout: None,
                        }),
                        "T" => m.exports.push(BmfExport {
                            kind: BmfKind::Template,
                            name: name.to_string(),
                            signature: rest.to_string(),
                            layout: None,
                        }),
                        _ => return None,
                    }
                }
                _ => return None,
            }
        }
        Some(m)
    }

    /// 查找导出 (按名)
    pub fn find_export(&self, name: &str) -> Option<&BmfExport> {
        self.exports.iter().find(|e| e.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_parse_round_trip() {
        let m = BmfModule {
            name: "dcc_test".to_string(),
            exports: vec![
                BmfExport {
                    kind: BmfKind::Function,
                    name: "answer".to_string(),
                    signature: "int()".to_string(),
                    layout: None,
                },
                BmfExport {
                    kind: BmfKind::Class,
                    name: "Widget".to_string(),
                    signature: String::new(),
                    layout: Some("id:int, score:double".to_string()),
                },
                BmfExport {
                    kind: BmfKind::Variable,
                    name: "g_counter".to_string(),
                    signature: "int".to_string(),
                    layout: None,
                },
            ],
            imports: vec!["std".to_string()],
        };
        let text = m.serialize();
        let parsed = BmfModule::parse(&text).expect("parse failed");
        assert_eq!(parsed, m);
    }

    #[test]
    fn parse_rejects_bad_magic() {
        assert!(BmfModule::parse("NOT_BMF_V1\nM x\n").is_none());
    }

    #[test]
    fn serialize_contains_magic_and_module_line() {
        let m = BmfModule {
            name: "m1".to_string(),
            exports: vec![],
            imports: vec![],
        };
        let text = m.serialize();
        assert!(text.starts_with("DCC_BMF_V1\nM m1\n"), "{}", text);
    }

    #[test]
    fn find_export_by_name() {
        let m = BmfModule {
            name: "m".to_string(),
            exports: vec![BmfExport {
                kind: BmfKind::Function,
                name: "f".to_string(),
                signature: "void(int)".to_string(),
                layout: None,
            }],
            imports: vec![],
        };
        assert!(m.find_export("f").is_some());
        assert!(m.find_export("g").is_none());
    }

    #[test]
    fn empty_class_layout_round_trips_as_none() {
        let m = BmfModule {
            name: "m".to_string(),
            exports: vec![BmfExport {
                kind: BmfKind::Class,
                name: "Empty".to_string(),
                signature: String::new(),
                layout: None,
            }],
            imports: vec![],
        };
        let parsed = BmfModule::parse(&m.serialize()).unwrap();
        assert_eq!(parsed, m);
    }
}
