//! C++20 modules BMF 序列化 (M26)
//!
//! BMF (Binary Module Format) 是 dcc 自定义的模块接口文件格式:
//! 记录模块名 + export 声明 (函数签名 / 类布局 / 变量) + import 列表,
//! 供跨 translation unit 合并模块接口 (M26.4: 真实跨 TU import).
//!
//! 实现: 零依赖文本格式, 前缀 magic `DCC_BMF_V1`, 每行一条记录:
//! ```text
//! DCC_BMF_V1
//! M <module-name>
//! P <partition-name>          ; 本 TU 属于 `module name:part;`
//! I <import-name>
//! X <partition-name>          ; import :part 分区 (其导出并入本模块)
//! E F <src-name> <full-c-decl>   ; export 函数 (decl 含 mangled 符号, 如
//!                                  `int _ZN6answerEv()`)
//! E C <class-name> <body-c>      ; export 类 (body 为单行 C struct 字段体)
//! E V <name> <full-c-decl>       ; export 变量
//! E T <template-name> <params>
//! ```
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
    /// M26.4: 本模块的分区名 (`module name:part;` 时 = Some("part"))
    pub partition: Option<String>,
    /// M26.4: 导入的分区 (`import :part;` -> "part"); 其导出并入本模块
    pub part_imports: Vec<String>,
}

impl BmfModule {
    /// 序列化为 BMF 文本
    pub fn serialize(&self) -> String {
        let mut out = format!("{}\n", BMF_MAGIC);
        out.push_str(&format!("M {}\n", self.name));
        if let Some(p) = &self.partition {
            out.push_str(&format!("P {}\n", p));
        }
        for i in &self.imports {
            out.push_str(&format!("I {}\n", i));
        }
        for x in &self.part_imports {
            out.push_str(&format!("X {}\n", x));
        }
        for e in &self.exports {
            match e.kind {
                BmfKind::Function => {
                    out.push_str(&format!("E F {} {}\n", e.name, e.signature));
                },
                BmfKind::Class => {
                    out.push_str(&format!(
                        "E C {} {}\n",
                        e.name,
                        e.layout.as_deref().unwrap_or("")
                    ));
                },
                BmfKind::Variable => {
                    out.push_str(&format!("E V {} {}\n", e.name, e.signature));
                },
                BmfKind::Template => {
                    // M26: 模板导出格式 E T <name> <params>
                    out.push_str(&format!("E T {} {}\n", e.name, e.signature));
                },
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
                "P" => m.partition = Some(parts.next()?.trim().to_string()),
                "I" => m.imports.push(parts.next()?.trim().to_string()),
                "X" => m.part_imports.push(parts.next()?.trim().to_string()),
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
                },
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
            partition: None,
            part_imports: vec![],
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
            partition: None,
            part_imports: vec![],
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
            partition: None,
            part_imports: vec![],
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
            partition: None,
            part_imports: vec![],
        };
        let parsed = BmfModule::parse(&m.serialize()).unwrap();
        assert_eq!(parsed, m);
    }

    #[test]
    fn partition_and_part_imports_round_trip() {
        // M26.4: `P` (partition 名) 与 `X` (分区导入) 行序列化/解析往返
        let m = BmfModule {
            name: "mymod".to_string(),
            exports: vec![BmfExport {
                kind: BmfKind::Function,
                name: "get".to_string(),
                signature: "int _ZN3getEv()".to_string(),
                layout: None,
            }],
            imports: vec![],
            partition: Some("part".to_string()),
            part_imports: vec!["part".to_string()],
        };
        let text = m.serialize();
        assert!(text.contains("P part\n"), "{}", text);
        assert!(text.contains("X part\n"), "{}", text);
        let parsed = BmfModule::parse(&text).unwrap();
        assert_eq!(parsed, m);
        assert_eq!(parsed.partition.as_deref(), Some("part"));
        assert_eq!(parsed.part_imports, vec!["part"]);
    }
}
