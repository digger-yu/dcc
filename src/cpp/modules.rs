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
//! - 找不到 BMF 时 -> **硬诊断 E0104** (M28.1: 旧的“当普通外部符号放行”与
//!   “回退为 `#include <foo>`”行为已全部删除, 不留逃生门)
//! - M26.5: 跨 TU ODR 检查 (本模块导出 vs 导入模块导出, 同名不同定义报错)
//!
//! ## native 路径 (driver 默认 C++ 后端) 的接口管道
//! driver 走 `cpp::native` (CppDecl -> HIR -> SSA -> ast::Program), 不经过
//! legacy `Lower` 的 C 文本, 因此接口的收集与注入都在 CppDecl 层完成:
//! - [`collect_interface`]: 从本 TU 的顶层声明收集 [`BmfModule`] (导出实体 =
//!   源码名 + C 签名文本 + 类字段布局; `module :private;` 之后停止收集)
//! - [`decls_from_bmf`]: 把 [`BmfModule`] 还原成 `is_decl` 纯声明 CppDecl
//!   (自由函数 / 全局变量 / 类 + 其 ctor/dtor/成员函数声明), splice 进用户
//!   AST 后即可被 sema 名字查找与 `lower_ir` 调用点降级使用
//! - [`odr_conflicts`]: 本 TU 导出 + 所有导入模块导出的 ODR 一致性检查
//!
//! 虚拟标记: 多态类的成员虚函数在 `E F` 的 signature 尾部追加 `@virtual`,
//! 使导入端重建同样的 vptr/vtable 布局 (不新增行格式, 保持 round-trip).
//!
//! ## M28: 接口完整性 (前一轮移交的 6 项缺口闭环)
//! - M28.1 `import m;` 找不到 BMF -> `error[E0104]: module 'm' not found`
//!   ([`resolve_imports`]), 不再静默按外部符号放行; `import <hdr>;` /
//!   `import "f";` 与「导入本 TU 自身模块名」不参与解析 (无诊断)
//! - M28.2 `module :private;` 片段实体 -> 内部链接 (STB_LOCAL), 并且**绝不
//!   进入 BMF 接口** (即使在 purview 里有向前声明): [`private_fragment_symbols`] +
//!   [`private_fragment_entity_names`] + [`ModuleLinkagePlan::apply`]
//! - M28.3 访问控制入 BMF: `E C` 记录 `@class`/`@struct`/`@union` 与每个字段的
//!   `@public`/`@private`/`@protected`, `E F` 尾部记录成员函数访问级 ->
//!   导入端跨 TU 访问 private 成员被 sema 拒绝
//! - M28.4 模板体入 BMF: `E T` 尾部 `@body=` 携带模板原文 (单行转义),
//!   导入端解析后注入 `CppDecl::Template`, 交给 native 模板实例化管线
//! - M28.5 继承布局入 BMF: `E C` 记录 `@base:<access>:<v>:<Name>`, 且收集时
//!   按基类虚函数表传播 `@virtual` (override 不写 virtual 也不丢虚槽);
//!   导入 TU **不发射** `_ZTV`/`_ZTI`/`_ZTS` (重放出的空壳虚表会在链接期
//!   与导出 TU 的 weak 定义随机二选一 -> 虚调用打空槽段错), 见 [`ModuleLinkagePlan::drop_data`];
//!   同一理由导入 TU 也**不定义**导入多态类的隐式 ctor/dtor (那份副本不写
//!   vptr), 调用解析到导出 TU 的真定义, 见 [`ModuleLinkagePlan::externalize`]
//! - M28.6 模块导出/导入实体的**发射符号名**统一走 Itanium mangled linkage
//!   name ([`module_linkage_plan`]), 消除 `Widget::get` 与全局 `get` 之类重名;
//!   ODR 键随之按 mangled 名比较 (`foo()` 与 `foo(int)` 不再是冲突)
//!
//! ## 剩余限制
//! - `import <header>` 行为等同 `#include <header>` (header 单元不做 BMF 解析)
//! - **隐式 ctor 的发射名不是 Itanium 名**: BMF 只记录用户声明的 ctor/dtor,
//!   隐式默认 ctor 在两侧都用后端约定名 (`Shape_ctor_complete`); 跨模块仍然
//!   自洽 (同名两边一致), 但与普通 TU 混用时不会得到 `_ZN5ShapeC1Ev`
//! - **无模块作用域 mangling**: GCC 把模块名编进链接名 (`_ZW5moda3foov`),
//!   dcc 用普通 Itanium 名 (`_Z3foov`). 两个模块导出“同 mangled 名 + 同签名”
//!   的实体时编译期视为同一实体 (不报 ODR), 链接期会是 multiple definition
//! - **函数体不入 BMF**: “同签名但定义体不同”的跨模块 ODR 违例无法静态检出
//!   (由 `cpp_odr_identical_imports_ok` 显式锁定该行为); 可检出的是签名、
//!   类布局与模板原文差异
//! - 命名空间内的实体不导出 (native 后端无跨 TU 的 ns 成员符号约定)
//! - M53: “接口里**裸** `export int f();` + 私有片段里定义”已修正: parser
//!   将单条声明的 `export decl` 编码为 `CppDecl::ExportBlock(vec![decl])`
//!   (与 `export { decl }` 同形, [module.global.frag] 里两者语义等价),
//!   故两种拼写的导出与链接级一致 (见单测
//!   `export_single_declaration_defined_in_private_fragment_stays_exported`)

use crate::cpp::bmf::{BmfExport, BmfKind, BmfModule};
use crate::cpp::mangle::{
    mangle_ctor, mangle_dtor, mangle_global_function, mangle_global_variable,
    mangle_member_function, MangleType, NestedName,
};
use crate::cpp::mangle::{encode_type_name, mangle_typeinfo, mangle_vtable};
use crate::cpp::parser::{
    member_decl_access, Access, BaseSpec, ClassDecl, CppBody, CppDecl, CppParser,
};

/// M28.1: 一条源码里的 `import` 语句 (按文本形态分类, parser 的
/// `CppDecl::Import{path}` 无法区分 `import m;` 与 `import <m>;`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportSite {
    /// 导入名: `import a.b;` -> `"a.b"`, `import :p;` -> `":p"`,
    /// `import <x/y>;` -> `"x/y"` 且 `is_header = true`
    pub name: String,
    /// `import <header>;` / `import "file";` —— header 单元, 不做 BMF 解析
    pub is_header: bool,
    /// 1-based 行号 (诊断定位)
    pub line: usize,
}

/// 扫描预处理后的源码文本里的全部 `import` 语句.
///
/// 只认顶层声明位置的 `import <词>` 形式 (`import` 前一是行首/空白/`;`/`:`/`}`,
/// 后随空白), 因此不会把标识符里的 `import` 子串误判为语句.
pub fn import_sites(src: &str) -> Vec<ImportSite> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut line = 1usize;
    let mut i = 0usize;
    let id_ok = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'$';
    while i < b.len() {
        match b[i] {
            b'\n' => {
                line += 1;
                i += 1;
                continue;
            },
            b'/' if i + 1 < b.len() && b[i + 1] == b'/' => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            },
            b'/' if i + 1 < b.len() && b[i + 1] == b'*' => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    if b[i] == b'\n' {
                        line += 1;
                    }
                    i += 1;
                }
                i = (i + 2).min(b.len());
                continue;
            },
            b'"' | b'\'' => {
                let q = b[i];
                i += 1;
                while i < b.len() && b[i] != q {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1;
                continue;
            },
            _ => {},
        }
        if b[i] == b'i'
            && src[i..].starts_with("import")
            && (i == 0 || !id_ok(b[i - 1]))
            && i + 6 < b.len()
            && !id_ok(b[i + 6])
        {
            let mut j = i + 6;
            while j < b.len() && (b[j] as char).is_whitespace() {
                j += 1;
            }
            let start_line = line + src.get(i..j).unwrap_or("").matches('\n').count();
            let mut is_header = false;
            let text: String;
            if j < b.len() && (b[j] == b'<' || b[j] == b'\"') {
                // header 单元: `import <a/b.h>;` / `import "x.h";`
                is_header = true;
                let close = if b[j] == b'<' { b'>' } else { b'\"' };
                j += 1;
                let start = j;
                while j < b.len() && b[j] != close {
                    j += 1;
                }
                text = src.get(start..j).unwrap_or("").to_string();
            } else {
                // 模块名 (含 `:partition`) 或限定名 `a.b`
                let start = j;
                while j < b.len()
                    && (id_ok(b[j]) || b[j] == b':' || b[j] == b'.' || b[j] == b'<')
                {
                    if b[j] == b'<' {
                        // `import std.vector;` 之后的模板参数不属实体名
                        break;
                    }
                    j += 1;
                }
                text = src.get(start..j).unwrap_or("").to_string();
            }
            if !text.is_empty() {
                out.push(ImportSite { name: text, is_header, line: start_line });
            }
            i = j;
            continue;
        }
        i += 1;
    }
    out
}

/// M28.1: 一个解析失败的模块导入.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingImport {
    /// 源码里的导入名 (`:part` 表示分区导入)
    pub name: String,
    /// 期望的 BMF 文件名
    pub file: String,
    pub line: usize,
    /// 尝试过的完整路径 (诊断信息)
    pub tried: Vec<std::path::PathBuf>,
}

/// M28.1: `import` 解析结果: 成功的 BMF + 失败的清单.
#[derive(Debug, Default, Clone)]
pub struct ImportResolution {
    pub loaded: Vec<BmfModule>,
    pub missing: Vec<MissingImport>,
}

/// 把 BMF 文件名解析为接口 (分区 -> `<模块名>.<分区>.bmf`).
///
/// `self_units` = 本 TU 自己 `export module` 出来的单元 (模块名, 分区名).
/// 导入这些单元不需要 BMF (接口就在本 TU 里, 单文件自洽); 其余导入必须
/// 能在搜索目录里找到 BMF, 否则记入 [`ImportResolution::missing`].
fn bmf_file_for(name: &str, module_name: &str, self_units: &[String]) -> Option<String> {
    if let Some(part) = name.strip_prefix(':') {
        // 分区导入: 必须知道本 TU 的模块名才能定位文件
        if module_name.is_empty() {
            return None;
        }
        if self_units.contains(&format!("{}.{}", module_name, part)) {
            return None;
        }
        return Some(format!("{}.{}.bmf", module_name, part));
    }
    // 导入本 TU 自身导出的模块名 (单文件接口单元) -> 无需 BMF
    if self_units.iter().any(|u| u.as_str() == name) {
        return None;
    }
    Some(format!("{}.bmf", name))
}

/// M28.1: 解析本 TU 的全部 `import` —— 这是取代“静默回退”的入口.
///
/// `src` 为预处理后的源码文本 (用于区分 `import m;` / `import <m>;`);
/// 传空串时退化为仅依 `CppDecl::Import` 记录判断 (旧行为).
/// 找不到的模块记入 [`ImportResolution::missing`], 由调用方 (driver)
/// 报 `error[E0104]` —— 不保留任何“忽略缺失”的逃生门.
pub fn resolve_imports(
    decls: &[CppDecl],
    search_dirs: &[std::path::PathBuf],
    src: &str,
) -> ImportResolution {
    // 本 TU 的模块名 (`export module m;` / `export module m:part;` 都是 m)
    let mut module_name = String::new();
    // 本 TU 自己导出的单元 (`export module`): `m` / `m.part`; 导入它们无需 BMF.
    // 实现单元 (`module m;` + `import m;`) 不在其中 —— 它必须读接口 BMF.
    let mut self_units: Vec<String> = Vec::new();
    for d in decls {
        if let CppDecl::Module { name: Some(n), partition, is_export } = d {
            if module_name.is_empty() {
                module_name = n.clone();
            }
            if *is_export {
                let u = match partition {
                    Some(p) => format!("{}.{}", n, p),
                    None => n.clone(),
                };
                if !self_units.contains(&u) {
                    self_units.push(u);
                }
            }
        }
    }
    // 待解析的导入名: 优先按源码文本 (能识别 header 形态), 否则按声明树
    let mut specs: Vec<(String, usize)> = Vec::new();
    let sites = import_sites(src);
    if !sites.is_empty() {
        specs = sites
            .into_iter()
            .filter(|s| !s.is_header)
            .map(|s| (s.name, s.line))
            .collect();
    } else {
        for d in decls {
            if let CppDecl::Import { path } = d {
                if path.is_empty() {
                    continue;
                }
                let name = path.join(".");
                // `import "x.h";` 在声明树里带引号, `import <x>;` 已在 src 扫描里处理
                if name.starts_with('\"') || name.contains('<') || name.contains('>') {
                    continue;
                }
                specs.push((name, 0));
            }
        }
    }
    let mut out = ImportResolution::default();
    let mut seen: Vec<String> = Vec::new();
    for (name, line) in specs {
        if seen.contains(&name) {
            continue;
        }
        seen.push(name.clone());
        let Some(file) = bmf_file_for(&name, &module_name, &self_units) else {
            continue;
        };
        let mut found: Option<(std::path::PathBuf, BmfModule)> = None;
        let mut tried = Vec::new();
        for dir in search_dirs {
            let p = dir.join(&file);
            tried.push(p.clone());
            if let Ok(text) = std::fs::read_to_string(&p) {
                match BmfModule::parse(&text) {
                    Some(m) => {
                        found = Some((p, m));
                        break;
                    },
                    None => {
                        // 文件存在但不是合法 BMF: 也算“找不到接口”, 但保留线索
                        found = None;
                        break;
                    },
                }
            }
        }
        match found {
            Some((_, m)) => out.loaded.push(m),
            None => out.missing.push(MissingImport { name, file, line, tried }),
        }
    }
    out
}

/// M26.4 旧 API (薄封装): 只返回加载成功的模块, 缺失项静默忽略.
///
/// driver 已改走 [`resolve_imports`] (缺失 -> E0104); 此入口仅留给单元测试
/// 与可能的存量调用方, 语义与历史一致.
pub fn load_imported_modules(
    decls: &[CppDecl],
    search_dirs: &[std::path::PathBuf],
) -> Vec<BmfModule> {
    resolve_imports(decls, search_dirs, "").loaded
}

/// M26.4: 修饰标记后缀. 记录在 `E F` 的 signature 尾部, 使导入端
/// 重建同样的虚槽 (vptr) / const 限定布局; 同时天然参与 ODR 签名比较
/// (虚/非虚同名同参是真实的布局冲突).
///
/// M28.3: 访问控制也用尾部标记 (`public` 为默认 -> 不写标记, 旧 BMF 文本
/// 逐字不变). M28.4: `@body=` 后的模板原文不在标记表里逐个拆, 而是整段取走
/// (原文含空格, 但已转义为单行).
const VIRTUAL_MARK: &str = "@virtual";
const CONST_MARK: &str = "@const";
const PRIVATE_MARK: &str = "@private";
const PROTECTED_MARK: &str = "@protected";
const PUBLIC_MARK: &str = "@public";
const BODY_MARK: &str = "@body=";
const CLASS_MARK: &str = "@class";
const STRUCT_MARK: &str = "@struct";
const UNION_MARK: &str = "@union";
const BASE_PREFIX: &str = "@base:";

/// 访问级 -> 标记文本 (public 不写, 保持旧 BMF 形式不变)
fn access_mark(a: Access) -> Option<&'static str> {
    match a {
        Access::Public => None,
        Access::Private => Some(PRIVATE_MARK),
        Access::Protected => Some(PROTECTED_MARK),
    }
}

/// 标记文本 -> 访问级
fn parse_access_mark(t: &str) -> Option<Access> {
    match t {
        PRIVATE_MARK => Some(Access::Private),
        PROTECTED_MARK => Some(Access::Protected),
        PUBLIC_MARK => Some(Access::Public),
        _ => None,
    }
}

fn access_word(a: Access) -> &'static str {
    match a {
        Access::Public => "public",
        Access::Private => "private",
        Access::Protected => "protected",
    }
}

fn word_access(w: &str) -> Access {
    match w {
        "private" => Access::Private,
        "protected" => Access::Protected,
        _ => Access::Public,
    }
}

/// M28.4: 模板原文 <-> 单行转义文本. 转义 `\` / 换行 / 制表 / `@`
/// (`@` 转义保证 `@body=` 标记在原文里不会被二次误认).
pub fn escape_body(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => {},
            '\t' => out.push_str("\\t"),
            '@' => out.push_str("\\@"),
            _ => out.push(c),
        }
    }
    out
}

/// [`escape_body`] 的逆
pub fn unescape_body(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut it = text.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => {},
            Some('\\') => out.push('\\'),
            Some('@') => out.push('@'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// M28.4: `E T` 的 signature = 模板形参列表 [+ ` @body=<单行转义原文>`]
pub fn make_template_signature(params: &str, body: Option<&str>) -> String {
    match body {
        Some(b) => format!("{} {}{}", params, BODY_MARK, escape_body(b)),
        None => params.to_string(),
    }
}

/// 拆 `E T` signature -> (模板形参列表, 已反转义的原文)
pub fn split_template_signature(sig: &str) -> (String, Option<String>) {
    match sig.find(BODY_MARK) {
        Some(pos) if pos > 0 && sig[..pos].ends_with(' ') => {
            let params = sig[..pos - 1].trim().to_string();
            let body = unescape_body(sig[pos + BODY_MARK.len()..].trim());
            (params, if body.is_empty() { None } else { Some(body) })
        },
        _ => (sig.trim().to_string(), None),
    }
}

/// MangleType -> C 类型文本 (BMF signature / layout 字段用).
/// 约定与 legacy `Lower::mangle_to_c_ns` 一致: 类写作 `struct Name`,
/// 指针 `T*`, 引用 `T&`, 数组 `T[N]`.
pub fn ty_to_c(ty: &MangleType) -> String {
    match ty {
        MangleType::Void => "void".to_string(),
        MangleType::Bool => "bool".to_string(),
        MangleType::Char => "char".to_string(),
        MangleType::Schar => "signed char".to_string(),
        MangleType::Uchar => "unsigned char".to_string(),
        MangleType::Short => "short".to_string(),
        MangleType::Ushort => "unsigned short".to_string(),
        MangleType::Int => "int".to_string(),
        MangleType::Uint => "unsigned int".to_string(),
        MangleType::Long => "long".to_string(),
        MangleType::Ulong => "unsigned long".to_string(),
        MangleType::LongLong => "long long".to_string(),
        MangleType::UlongLong => "unsigned long long".to_string(),
        MangleType::Float => "float".to_string(),
        MangleType::Double => "double".to_string(),
        MangleType::LongDouble => "long double".to_string(),
        MangleType::Ellipsis => "...".to_string(),
        MangleType::Pointer(inner) => format!("{}*", ty_to_c(inner)),
        MangleType::Reference(inner) => format!("{}&", ty_to_c(inner)),
        MangleType::Array(inner, n) => format!("{}[{}]", ty_to_c(inner), n),
        MangleType::Class(n) => {
            if n.starts_with("struct ") || n.starts_with("union ") || n.starts_with("enum ") {
                n.clone()
            } else {
                format!("struct {}", n)
            }
        },
        MangleType::Template { name, args } => {
            let a: Vec<String> = args.iter().map(ty_to_c).collect();
            format!("struct {}<{}>", name, a.join(", "))
        },
        _ => "int".to_string(),
    }
}

/// C 类型文本 -> MangleType (`ty_to_c` 的逆; 未知具名类型按类处理)
pub fn c_to_ty(text: &str) -> Option<MangleType> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    if let Some(inner) = t.strip_suffix('*') {
        return Some(MangleType::Pointer(Box::new(c_to_ty(inner)?)));
    }
    if let Some(inner) = t.strip_suffix('&') {
        return Some(MangleType::Reference(Box::new(c_to_ty(inner)?)));
    }
    if let Some(open) = t.rfind('[') {
        if t.ends_with(']') {
            let n: usize = t[open + 1..t.len() - 1].trim().parse().ok()?;
            return Some(MangleType::Array(Box::new(c_to_ty(&t[..open])?), n));
        }
    }
    if t.starts_with("struct ") || t.starts_with("union ") || t.starts_with("enum ") {
        return Some(MangleType::Class(t[7..].trim().to_string()));
    }
    match t {
        "void" => Some(MangleType::Void),
        "bool" => Some(MangleType::Bool),
        "char" => Some(MangleType::Char),
        "signed char" => Some(MangleType::Schar),
        "unsigned char" => Some(MangleType::Uchar),
        "short" => Some(MangleType::Short),
        "unsigned short" => Some(MangleType::Ushort),
        "int" => Some(MangleType::Int),
        "unsigned" | "unsigned int" => Some(MangleType::Uint),
        "long" => Some(MangleType::Long),
        "long long" => Some(MangleType::LongLong),
        "unsigned long" => Some(MangleType::Ulong),
        "unsigned long long" => Some(MangleType::UlongLong),
        "float" => Some(MangleType::Float),
        "double" => Some(MangleType::Double),
        "long double" => Some(MangleType::LongDouble),
        "..." => Some(MangleType::Ellipsis),
        _ => {
            // 具名类型: 模板实例 `Foo<int>` 或普通类名
            if let Some(lt) = t.find('<') {
                if t.ends_with('>') {
                    let name = t[..lt].trim().to_string();
                    let inner = &t[lt + 1..t.len() - 1];
                    let mut args = Vec::new();
                    for a in split_top_level(inner) {
                        if a.trim() == "..." {
                            continue;
                        }
                        args.push(c_to_ty(&a)?);
                    }
                    return Some(MangleType::Template { name, args });
                }
            }
            Some(MangleType::Class(t.to_string()))
        },
    }
}

/// 按顶层逗号切分 (忽略 `<>` / `()` / `[]` 内部的逗号)
fn split_top_level(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '<' | '(' | '[' => {
                depth += 1;
                cur.push(c);
            },
            '>' | ')' | ']' => {
                depth -= 1;
                cur.push(c);
            },
            ',' if depth == 0 => {
                out.push(std::mem::take(&mut cur));
            },
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// 生成 BMF 函数签名: `ret mangled(T1, T2)` [+ ` @virtual`] [+ ` @const`]
pub fn make_fn_signature(
    ret: &MangleType,
    mangled: &str,
    params: &[MangleType],
    is_virtual: bool,
) -> String {
    make_fn_signature_ex(ret, mangled, params, is_virtual, false)
}

/// [`make_fn_signature`] 的完整形式: 额外记录成员函数的 const 限定
pub fn make_fn_signature_ex(
    ret: &MangleType,
    mangled: &str,
    params: &[MangleType],
    is_virtual: bool,
    is_const: bool,
) -> String {
    make_fn_signature_full(ret, mangled, params, is_virtual, is_const, None)
}

/// M28.3: 完整形式 —— 额外记录成员函数的访问级 (`None`/`Public` = 不写标记,
/// 因此 public 成员的签名文本与旧 BMF 逐字一致)
pub fn make_fn_signature_full(
    ret: &MangleType,
    mangled: &str,
    params: &[MangleType],
    is_virtual: bool,
    is_const: bool,
    access: Option<Access>,
) -> String {
    let args: Vec<String> = params.iter().map(ty_to_c).collect();
    let mut s = format!("{} {}({})", ty_to_c(ret), mangled, args.join(", "));
    if is_virtual {
        s.push(' ');
        s.push_str(VIRTUAL_MARK);
    }
    if is_const {
        s.push(' ');
        s.push_str(CONST_MARK);
    }
    if let Some(m) = access.and_then(access_mark) {
        s.push(' ');
        s.push_str(m);
    }
    s
}

/// 剥离签名尾部的修饰标记 -> (签名主体, is_virtual, is_const, access)
fn split_marks(text: &str) -> (String, bool, bool, Option<Access>) {
    let mut s = text.trim().to_string();
    let mut is_virtual = false;
    let mut is_const = false;
    let mut access: Option<Access> = None;
    loop {
        match s.rsplit_once(char::is_whitespace) {
            Some((head, tail)) if tail == VIRTUAL_MARK && !is_virtual => {
                s = head.trim().to_string();
                is_virtual = true;
            },
            Some((head, tail)) if tail == CONST_MARK && !is_const => {
                s = head.trim().to_string();
                is_const = true;
            },
            Some((head, tail)) if access.is_none() && parse_access_mark(tail).is_some() => {
                access = parse_access_mark(tail);
                s = head.trim().to_string();
            },
            _ => break,
        }
    }
    (s, is_virtual, is_const, access)
}

/// M28.3/M28.6: 一条 `E F` 签名解出的全部信息
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FnSigInfo {
    pub ret: MangleType,
    pub params: Vec<MangleType>,
    /// 签名里的 mangled 符号名 (旧 BMF 可能没有 -> None)
    pub mangled: Option<String>,
    pub is_virtual: bool,
    pub is_const: bool,
    pub access: Option<Access>,
}

/// 解析 BMF 函数签名 (全量形式: 返回/形参/虚槽/const/访问级)
///
/// 形如 `int _ZN5twiceEi(int)` / `void _ZN6WidgetC1Ei(struct Widget*, int) @virtual`.
/// 无法解析返回 None (调用方跳过该导出, 不阻断编译).
pub fn parse_fn_signature_full(text: &str) -> Option<FnSigInfo> {
    let (text, is_virtual, is_const, access) = split_marks(text);
    let open = text.find('(')?;
    if !text.ends_with(')') {
        return None;
    }
    let close = text.rfind(')')?;
    if close < open {
        return None;
    }
    let head = text[..open].trim();
    let args = text[open + 1..close].trim();
    // head = `<ret 可能多词> <函数名 (mangled)>`; `*` / `&` 附着于返回类型时
    // (如 `int* _Z...`) 仍按最后一个空白分隔的 token 视为函数名.
    let mut ht: Vec<&str> = head.split_whitespace().collect();
    if ht.is_empty() {
        return None;
    }
    let name = ht.pop().unwrap_or("").to_string();
    let mangled = if name.starts_with("_Z") { Some(name) } else { None };
    let ret = c_to_ty(&ht.join(" "))?;
    let mut params = Vec::new();
    if !args.is_empty() && args != "void" {
        for a in split_top_level(args) {
            params.push(c_to_ty(a.trim())?);
        }
    }
    Some(FnSigInfo { ret, params, mangled, is_virtual, is_const, access })
}

/// 解析 BMF 函数签名 -> (返回类型, 形参类型, 是否 virtual)
pub fn parse_fn_signature(text: &str) -> Option<(MangleType, Vec<MangleType>, bool)> {
    let i = parse_fn_signature_full(text)?;
    Some((i.ret, i.params, i.is_virtual))
}

/// [`parse_fn_signature`] 的完整形式 (含 const 限定)
pub fn parse_fn_signature_ex(text: &str) -> Option<(MangleType, Vec<MangleType>, bool, bool)> {
    let i = parse_fn_signature_full(text)?;
    Some((i.ret, i.params, i.is_virtual, i.is_const))
}

/// M28.6: 取签名里的 mangled 符号名 (ODR 键 / linkage 改名表的单一来源)
pub fn signature_mangled(text: &str) -> Option<String> {
    parse_fn_signature_full(text).and_then(|i| i.mangled)
}

/// M28.3: 一个导出字段 (名 + 类型 + 访问级)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BmfField {
    pub name: String,
    pub ty: MangleType,
    pub access: Access,
}

/// M28.3/M28.5: `E C` 布局记录解出的全部信息
///
/// (`BaseSpec` 在 parser 侧没有 `PartialEq`, 故本结构不 derive 比较; 测试按字段断言)
#[derive(Debug, Clone)]
pub struct BmfClassLayout {
    pub is_struct: bool,
    pub is_union: bool,
    pub bases: Vec<BaseSpec>,
    pub fields: Vec<BmfField>,
}

/// 生成 `E C` 的 layout 文本:
/// `[@class|@struct|@union] [@base:<acc>:<v|->:<N>]* [@<acc> <ty> <name>;]*`
///
/// 旧 BMF (`int x; int y;`) 没有这些标记, [`parse_layout_ex`] 按 `struct`
/// (成员全 public) 回退 —— 即历史行为, 旧接口文件仍可读.
pub fn encode_class_layout(
    is_struct: bool,
    is_union: bool,
    bases: &[BaseSpec],
    fields: &[BmfField],
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if is_union {
        parts.push(UNION_MARK.to_string());
    } else if is_struct {
        parts.push(STRUCT_MARK.to_string());
    } else {
        parts.push(CLASS_MARK.to_string());
    }
    for b in bases {
        let v = if b.is_virtual { "v" } else { "-" };
        parts.push(format!("{}{}:{}:{}", BASE_PREFIX, access_word(b.access), v, b.name));
    }
    for f in fields {
        match access_mark(f.access) {
            Some(m) => parts.push(format!("{} {} {};", m, ty_to_c(&f.ty), f.name)),
            None => parts.push(format!("{} {};", ty_to_c(&f.ty), f.name)),
        }
    }
    parts.join(" ")
}

/// M28.5: `@base:<access>:<v|->:<Name>` -> BaseSpec
fn parse_base_token(t: &str) -> Option<BaseSpec> {
    let rest = t.strip_prefix(BASE_PREFIX)?;
    let mut it = rest.splitn(3, ':');
    let access = word_access(it.next()?);
    let virt = it.next()?;
    let raw = it.next().unwrap_or("");
    if raw.is_empty() {
        return None;
    }
    // `Base<int>`: 基类名不含模板实参 (与 parser 的 BaseSpec 约定一致)
    let (name, args) = match raw.find('<') {
        Some(lt) if raw.ends_with('>') => {
            let inner = &raw[lt + 1..raw.len() - 1];
            let mut a = Vec::new();
            for s in split_top_level(inner) {
                if let Some(ty) = c_to_ty(s.trim()) {
                    a.push(ty);
                }
            }
            (raw[..lt].trim().to_string(), a)
        },
        _ => (raw.to_string(), Vec::new()),
    };
    Some(BaseSpec {
        access,
        is_virtual: virt == "v",
        name,
        template_args: if args.is_empty() { None } else { Some(args) },
    })
}

/// 解析 `E C` 布局文本 (M28.3/M28.5 全量形式; 旧文本按 struct/public 回退)
pub fn parse_layout_ex(layout: &str) -> BmfClassLayout {
    let mut toks = layout.split_whitespace().peekable();
    let mut is_struct = true;
    let mut is_union = false;
    let mut bases: Vec<BaseSpec> = Vec::new();
    while let Some(t) = toks.peek().copied() {
        if t == CLASS_MARK {
            is_struct = false;
            toks.next();
        } else if t == STRUCT_MARK {
            is_struct = true;
            toks.next();
        } else if t == UNION_MARK {
            is_struct = true;
            is_union = true;
            toks.next();
        } else if t.starts_with(BASE_PREFIX) {
            if let Some(b) = parse_base_token(t) {
                bases.push(b);
            }
            toks.next();
        } else {
            break;
        }
    }
    // 默认访问级: **public**. `access_mark(Public) == None` —— 新收集器不给
    // public 成员写标记, 所以“无标记”就代表 public; 若退回语言默认
    // (`class` -> private) 会使导入端把 public 成员误判为私有.
    // 旧 BMF (无任何标记) 因此也按全 public 回放 = M26 历史行为.
    let default_access = Access::Public;
    let mut fields: Vec<BmfField> = Vec::new();
    let rest: String = toks.collect::<Vec<_>>().join(" ");
    for part in rest.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let mut tk: Vec<&str> = part.split_whitespace().collect();
        let mut access = default_access;
        if tk.first().is_some_and(|t| parse_access_mark(t).is_some()) {
            access = parse_access_mark(tk.remove(0)).unwrap_or(default_access);
        }
        if tk.len() < 2 {
            continue;
        }
        let name = tk.pop().unwrap().to_string();
        let ty = tk.join(" ");
        if let Some(t) = c_to_ty(&ty) {
            fields.push(BmfField { name, ty: t, access });
        }
    }
    BmfClassLayout { is_struct, is_union, bases, fields }
}

/// 解析 BMF 类布局文本 `int id; double x;` -> [(字段名, 类型)]
///
/// 旧 API (M26): 不读访问级 / 基类标记; 新代码用 [`parse_layout_ex`].
pub fn parse_layout(layout: &str) -> Vec<(String, MangleType)> {
    parse_layout_ex(layout)
        .fields
        .into_iter()
        .map(|f| (f.name, f.ty))
        .collect()
}

/// 纯声明 body (`is_decl`): 导入端只声明接口, 定义在导出 TU 的目标文件里
fn decl_body() -> CppBody {
    CppBody { stmts: Vec::new(), is_decl: true }
}

/// 收集本 TU 的模块接口 (BMF). 无 `export module name;` 声明时返回 None.
///
/// 收集约定 (与 legacy `Lower` 的模块收集模式一致):
/// - `export module name;` / `export module name:part;` 进入收集模式;
///   `module name;` (实现单元) 不产出接口
/// - `module :private;` 终止收集 (私有片段不导出, M28.2 另施加内部链接)
/// - `main` 永不导出
/// - `export { ... }` 块内声明递归收集
pub fn collect_interface(decls: &[CppDecl]) -> Option<BmfModule> {
    collect_interface_src(decls, None)
}

/// [`collect_interface`] 的 M28.4 形式: 传入预处理后的源码文本时,
/// 导出的模板会把原文随 `E T` 的 `@body=` 带走 (导入端可重建实例).
pub fn collect_interface_src(decls: &[CppDecl], src: Option<&str>) -> Option<BmfModule> {
    let mut bmf = BmfModule::default();
    let mut collect = false;
    let mut seen_module = false;
    // M28.5: 基类虚函数表 —— override 不写 `virtual` 也不能丢虚槽
    let vidx = build_virtual_index(decls);
    for d in decls {
        match d {
            CppDecl::Module { name, partition, is_export } => {
                if let Some(n) = name {
                    bmf.name = n.clone();
                    bmf.partition = partition.clone();
                    collect = *is_export;
                    seen_module = true;
                } else {
                    // `module;` = global module fragment
                }
            },
            CppDecl::Import { path } => {
                if path.len() == 1 {
                    if let Some(part) = path[0].strip_prefix(':') {
                        if !bmf.part_imports.contains(&part.to_string()) {
                            bmf.part_imports.push(part.to_string());
                        }
                    } else if !bmf.imports.contains(&path[0]) {
                        bmf.imports.push(path[0].clone());
                    }
                }
            },
            CppDecl::PrivateFragment => collect = false,
            CppDecl::ExportBlock(inner) => {
                if collect {
                    collect_export_block(inner, &mut bmf.exports, src, &vidx);
                }
            },
            CppDecl::Namespace { body, .. } | CppDecl::InlineNamespace { body, .. } => {
                // 命名空间内实体的名字尚不含 ns 前缀, native 后端也未支持
                // 跨 TU 的 ns 成员; 记录但不下钻, 避免导出无法回放的实体.
                let _ = body;
            },
            other => {
                if collect {
                    push_export(other, &mut bmf.exports, src, &vidx);
                }
            },
        }
    }
    if !seen_module || bmf.name.is_empty() {
        return None;
    }
    // M28.2: 私有片段实体**绝不属于模块接口**. 实体在片段内定义、却因向前声明
    // 出现在 purview 时, 上面的收集会把它误记成 `E` 导出 -> 导入端会重放出
    // 一个引用, 而其定义已是 STB_LOCAL, 链接期必炸. 按源名统一剔除
    // (类成员按 `Class::m` 的主名匹配).
    let pv = private_fragment_entity_names(decls);
    if !pv.is_empty() {
        bmf.exports.retain(|e| {
            let root = e.name.split("::").next().unwrap_or("");
            !(pv.contains(&e.name) || (!root.is_empty() && pv.contains(root)))
        });
    }
    Some(bmf)
}

/// M28.2: `module :private;` 之后声明的实体**源名** (供 BMF 剔除用).
///
/// 与 [`private_fragment_symbols`] 的区别: 后者给 native 产物的约定符号名
/// (ctor/dtor/成员函数变体名), 本函数给 BMF `E` 记录里的源级名字.
fn private_fragment_entity_names(decls: &[CppDecl]) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for d in private_region_decls(decls) {
        if let Some(n) = entity_source_name(d) {
            out.insert(n);
        }
    }
    out
}

/// `export { ... }` 块: 块内顶层声明按导出处理 (块本身已在收集模式内)
fn collect_export_block(
    decls: &[CppDecl],
    out: &mut Vec<BmfExport>,
    src: Option<&str>,
    vidx: &VirtualIndex,
) {
    for d in decls {
        match d {
            CppDecl::ExportBlock(inner) => collect_export_block(inner, out, src, vidx),
            other => push_export(other, out, src, vidx),
        }
    }
}

/// 单条顶层声明 -> BMF 导出记录
fn push_export(
    d: &CppDecl,
    out: &mut Vec<BmfExport>,
    src: Option<&str>,
    vidx: &VirtualIndex,
) {
    match d {
        CppDecl::FreeFn { ret, name, params, trailing_ret_type, .. } => {
            if name == "main" {
                return;
            }
            let r = trailing_ret_type.as_ref().unwrap_or(ret);
            let ptys: Vec<MangleType> = params.iter().map(|(_, t)| t.clone()).collect();
            let m = mangle_global_function(name, &ptys);
            out.push(BmfExport {
                kind: BmfKind::Function,
                name: name.clone(),
                signature: make_fn_signature(r, &m, &ptys, false),
                layout: None,
            });
        },
        CppDecl::GlobalVar { ty, name, .. } => {
            out.push(BmfExport {
                kind: BmfKind::Variable,
                name: name.clone(),
                signature: ty_to_c(ty),
                layout: None,
            });
        },
        CppDecl::Template { decl, is_export, params, .. } => {
            if !*is_export {
                return;
            }
            let tn = match decl.as_ref() {
                CppDecl::Class(c) => c.name.clone(),
                CppDecl::FreeFn { name, .. } => name.clone(),
                _ => String::new(),
            };
            if tn.is_empty() {
                return;
            }
            let plist = params
                .params
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(",");
            // M28.4: 模板体序列化 (源文本级回放)
            let body = src.and_then(|s| find_template_src(s, &tn));
            out.push(BmfExport {
                kind: BmfKind::Template,
                name: tn,
                signature: make_template_signature(&plist, body.as_deref()),
                layout: None,
            });
        },
        CppDecl::Class(c) => out.extend(class_exports(c, vidx)),
        _ => {},
    }
}

/// 一个类 -> `E C` 布局记录 (含 M28.3 访问级 / M28.5 基类) +
/// 每个 ctor/dtor/成员函数一条 `E F Class::m`
fn class_exports(c: &ClassDecl, vidx: &VirtualIndex) -> Vec<BmfExport> {
    let mut out = Vec::new();
    let mut fields: Vec<BmfField> = Vec::new();
    let mut members = Vec::new();
    // 类内声明的 `class` 字段偶尔为空 (parser 容错路径), 统一回退到类名,
    // 否则 this 类型与 mangled 名会缺主类限定.
    let qual = |n: &str| -> String {
        if n.is_empty() {
            c.name.clone()
        } else {
            n.to_string()
        }
    };
    for m in &c.members {
        let access = member_decl_access(c, m);
        match m {
            CppDecl::MemberVar { ty, name, .. } => {
                fields.push(BmfField { name: name.clone(), ty: ty.clone(), access });
            },
            CppDecl::Ctor { class, params, is_virtual, .. } => {
                let class = qual(class);
                let ptys: Vec<MangleType> = params.iter().map(|(_, t)| t.clone()).collect();
                let this = MangleType::Pointer(Box::new(MangleType::Class(class.clone())));
                let mut all = vec![("this".to_string(), this)];
                all.extend(params.iter().cloned());
                let alltys: Vec<MangleType> = all.iter().map(|(_, t)| t.clone()).collect();
                let n = NestedName::from_dotted(vec![class.clone()]);
                let mg = mangle_ctor(&n, &ptys, 1);
                members.push(BmfExport {
                    kind: BmfKind::Function,
                    name: format!("{}::ctor", class),
                    signature: make_fn_signature_full(
                        &MangleType::Void,
                        &mg,
                        &alltys,
                        *is_virtual,
                        false,
                        Some(access),
                    ),
                    layout: None,
                });
            },
            CppDecl::Dtor { class, is_virtual, .. } => {
                let class = qual(class);
                let this = MangleType::Pointer(Box::new(MangleType::Class(class.clone())));
                let n = NestedName::from_dotted(vec![class.clone()]);
                let mg = mangle_dtor(&n, 1);
                members.push(BmfExport {
                    kind: BmfKind::Function,
                    name: format!("{}::dtor", class),
                    signature: make_fn_signature_full(
                        &MangleType::Void,
                        &mg,
                        &[this],
                        *is_virtual,
                        false,
                        Some(access),
                    ),
                    layout: None,
                });
            },
            CppDecl::MemberFn { class, ret, name, params, is_virtual, is_const, .. } => {
                let class = qual(class);
                let ptys: Vec<MangleType> = params.iter().map(|(_, t)| t.clone()).collect();
                // 成员函数签名首参为 this 指针 (与 legacy Lower 一致); 导入端
                // 重建声明时剔除, 由后端隐式补齐
                let this = MangleType::Pointer(Box::new(MangleType::Class(class.clone())));
                let mut all: Vec<MangleType> = vec![this];
                all.extend(ptys.iter().cloned());
                let n = NestedName::from_dotted(vec![class.clone(), name.clone()]);
                let mg = mangle_member_function(&n, &ptys, ret);
                // M28.5: `int area();` 在 `Shape` 已有 `virtual int area()` 时
                // 仍是虚函数 (override 隐式继承 virtual), 必须记 @virtual,
                // 否则导入端重建的 vtable 与导出端槽位数不一致.
                let virt =
                    *is_virtual || vidx.is_virtual(&class, &vfunc_key(name, &ptys));
                members.push(BmfExport {
                    kind: BmfKind::Function,
                    name: format!("{}::{}", class, name),
                    signature: make_fn_signature_full(
                        ret,
                        &mg,
                        &all,
                        virt,
                        *is_const,
                        Some(access),
                    ),
                    layout: None,
                });
            },
            _ => {},
        }
    }
    let layout = encode_class_layout(c.is_struct, c.is_union, &c.bases, &fields);
    out.push(BmfExport {
        kind: BmfKind::Class,
        name: c.name.clone(),
        signature: String::new(),
        layout: Some(layout),
    });
    out.extend(members);
    out
}

/// M28.5: 虚函数键 (方法名 + 形参类型文本; 不含返回类型, 与重写规则一致)
fn vfunc_key(name: &str, params: &[MangleType]) -> String {
    let a: Vec<String> = params.iter().map(ty_to_c).collect();
    format!("{}({})", name, a.join(","))
}

/// 递归收集 TU 内所有类声明 (含 `export {}` 块与模板体内的类)
fn gather_classes<'a>(decls: &'a [CppDecl], out: &mut Vec<&'a ClassDecl>) {
    for d in decls {
        match d {
            CppDecl::Class(c) => out.push(c),
            CppDecl::ExportBlock(inner) => gather_classes(inner, out),
            CppDecl::Template { decl, .. } => gather_classes(std::slice::from_ref(decl), out),
            CppDecl::Namespace { body, .. } | CppDecl::InlineNamespace { body, .. } => {
                gather_classes(body, out)
            },
            _ => {},
        }
    }
}

/// M28.5: TU 内「类 -> 虚函数键集合」索引 (沿基类传递闭包).
#[derive(Debug, Default)]
struct VirtualIndex {
    map: std::collections::HashMap<String, std::collections::HashSet<String>>,
}

impl VirtualIndex {
    fn is_virtual(&self, class: &str, key: &str) -> bool {
        self.map.get(class).is_some_and(|s| s.contains(key))
    }
}

fn build_virtual_index(decls: &[CppDecl]) -> VirtualIndex {
    let mut classes: Vec<&ClassDecl> = Vec::new();
    gather_classes(decls, &mut classes);
    let mut own: std::collections::HashMap<String, std::collections::HashSet<String>> =
        std::collections::HashMap::new();
    let mut bases: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for c in &classes {
        let e = own.entry(c.name.clone()).or_default();
        for m in &c.members {
            if let CppDecl::MemberFn { name, params, is_virtual: v, .. } = m {
                if *v {
                    let ptys: Vec<MangleType> = params.iter().map(|(_, t)| t.clone()).collect();
                    e.insert(vfunc_key(name, &ptys));
                }
            }
        }
        bases
            .entry(c.name.clone())
            .or_default()
            .extend(c.bases.iter().map(|b| b.name.clone()));
    }
    // 不动点传播: 基类的虚键下行到派生类 (基类在后声明也能收敛)
    let mut idx = VirtualIndex { map: own };
    loop {
        let mut changed = false;
        for (class, bs) in &bases {
            let mut add: Vec<String> = Vec::new();
            for b in bs {
                if let Some(set) = idx.map.get(b) {
                    for k in set {
                        if !idx.map.get(class).is_some_and(|s| s.contains(k)) {
                            add.push(k.clone());
                        }
                    }
                }
            }
            let e = idx.map.entry(class.clone()).or_default();
            for k in add {
                if e.insert(k) {
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    idx
}

/// M28.4: 在源码文本里定位名为 `name` 的模板声明原文.
///
/// `CppDecl::Template` 不携带 span/偏移 (parser 域, 本轮不改), 所以下标靠文本
/// 扫描: `template` 关键字 -> 尖括号配对的形参表 -> 大括号配对 (或 `;`) 的体.
/// 取第一个“声明头里出现该名字”的匹配.
pub fn find_template_src(src: &str, name: &str) -> Option<String> {
    let b = src.as_bytes();
    let id_ok = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'$';
    let mut from = 0usize;
    while let Some(p) = src[from..].find("template") {
        let start = from + p;
        from = start + 1;
        if start > 0 && id_ok(b[start - 1]) {
            continue;
        }
        let after = start + "template".len();
        if after < b.len() && id_ok(b[after]) {
            continue;
        }
        let Some(len) = template_decl_len(&src[start..]) else { continue };
        let text = &src[start..start + len];
        // 声明头 (去掉体) 里必须出现该名字, 才是我们要的模板
        let head = match text.find('{') {
            Some(ob) => &text[..ob],
            None => text,
        };
        if head
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
            .any(|t| t == name || t.ends_with(&format!("::{}", name)) || t == name)
        {
            return Some(text.to_string());
        }
    }
    None
}

/// 从 `template` 关键字起, 返回该模板声明 (含体) 的字符长度
fn template_decl_len(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = "template".len();
    let skip_ws = |i: &mut usize, b: &[u8]| {
        while *i < b.len() && (b[*i] as char).is_whitespace() {
            *i += 1;
        }
    };
    skip_ws(&mut i, b);
    if i >= b.len() || b[i] != b'<' {
        return None;
    }
    // 形参表: 尖括号配对
    let mut depth = 0i32;
    while i < b.len() {
        match b[i] {
            b'<' => depth += 1,
            b'>' => {
                depth -= 1;
                if depth == 0 {
                    i += 1;
                    break;
                }
            },
            _ => {},
        }
        i += 1;
    }
    if depth != 0 {
        return None;
    }
    // 声明头 -> 首个 `{` 为体, 否则到 `;` 结束 (声明式模板)
    let mut curly = 0i32;
    let mut seen_body = false;
    while i < b.len() {
        match b[i] {
            b'{' => {
                curly += 1;
                seen_body = true;
            },
            b'}' => {
                curly -= 1;
                if curly == 0 && seen_body {
                    i += 1;
                    // 尾随 `;` (类定义后的分号)
                    if i < b.len() && b[i] == b';' {
                        i += 1;
                    }
                    return Some(i);
                }
            },
            b';' if curly == 0 && !seen_body => {
                return Some(i + 1);
            },
            _ => {},
        }
        i += 1;
    }
    None
}

/// 把 BMF 接口还原为可 splice 进用户 AST 的 C++ 顶层声明.
///
/// 全部为纯声明 (`is_decl`): 导入 TU 只引用符号, 定义留在导出 TU 的 .o.
/// M28.3: 类的 `class`/`struct`/`union` 属性、字段与成员访问级、M28.5: 基类
/// 列表均从 `E C` 布局标记还原 (旧 BMF 无标记 -> 按 struct/全 public 回退,
/// 即历史行为); ctor / dtor / 成员函数挂回所属类, `virtual` 与访问级由
/// `E F` 签名尾部标记还原. M28.4: `E T` 带 `@body=` 时重建模板声明,
/// 交给 native 模板实例化管线.
pub fn decls_from_bmf(bmf: &BmfModule) -> Vec<CppDecl> {
    /// 一个类的回放累加器
    struct Replay {
        members: Vec<CppDecl>,
        access: Vec<(String, Access)>,
        is_struct: bool,
        is_union: bool,
        bases: Vec<BaseSpec>,
    }
    // 类名 -> 成员声明 (保持插入顺序)
    let mut order: Vec<String> = Vec::new();
    let mut classes: std::collections::BTreeMap<String, Replay> =
        std::collections::BTreeMap::new();
    let mut free: Vec<CppDecl> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    fn entry_of<'a>(
        name: &str,
        classes: &'a mut std::collections::BTreeMap<String, Replay>,
    ) -> &'a mut Replay {
        classes.entry(name.to_string()).or_insert_with(|| Replay {
            members: Vec::new(),
            access: Vec::new(),
            is_struct: true,
            is_union: false,
            bases: Vec::new(),
        })
    }

    for e in &bmf.exports {
        match e.kind {
            BmfKind::Class => {
                if !seen.insert(format!("C {}", e.name)) {
                    continue;
                }
                if !order.contains(&e.name) {
                    order.push(e.name.clone());
                }
                let lay = parse_layout_ex(e.layout.as_deref().unwrap_or(""));
                let r = entry_of(&e.name, &mut classes);
                r.is_struct = lay.is_struct;
                r.is_union = lay.is_union;
                r.bases = lay.bases;
                for f in lay.fields {
                    r.access.push((f.name.clone(), f.access));
                    r.members.push(CppDecl::MemberVar {
                        ty: f.ty,
                        name: f.name,
                        is_mutable: false,
                        is_volatile: false,
                    });
                }
            },
            BmfKind::Function => {
                if !seen.insert(format!("F {} {}", e.name, e.signature)) {
                    continue;
                }
                if let Some((class, meth)) = e.name.split_once("::") {
                    if !order.contains(&class.to_string()) {
                        order.push(class.to_string());
                    }
                    let info = match parse_fn_signature_full(&e.signature) {
                        Some(v) => v,
                        None => continue,
                    };
                    let mut tys = info.params.clone();
                    // 签名首参是 this 指针, 成员声明里由后端隐式补齐
                    if matches!(&tys.first(),
                        Some(MangleType::Pointer(inner))
                        if matches!(inner.as_ref(), MangleType::Class(c) if *c == class))
                    {
                        tys.remove(0);
                    }
                    let params: Vec<(String, MangleType)> = tys
                        .into_iter()
                        .enumerate()
                        .map(|(i, t)| (format!("__p{}", i), t))
                        .collect();
                    let (key, decl) = match meth {
                        "ctor" => (
                            class.to_string(),
                            CppDecl::Ctor {
                                class: class.to_string(),
                                params,
                                inits: Vec::new(),
                                body: decl_body(),
                                is_virtual: false,
                                is_explicit: false,
                                is_noexcept: false,
                            },
                        ),
                        "dtor" => (
                            format!("~{}", class),
                            CppDecl::Dtor {
                                class: class.to_string(),
                                body: decl_body(),
                                is_virtual: info.is_virtual,
                                is_noexcept: false,
                            },
                        ),
                        _ => (
                            meth.to_string(),
                            CppDecl::MemberFn {
                                class: class.to_string(),
                                ret: info.ret.clone(),
                                name: meth.to_string(),
                                params,
                                body: decl_body(),
                                is_virtual: info.is_virtual,
                                is_override: false,
                                is_final: false,
                                is_const: info.is_const,
                                is_noexcept: false,
                                is_volatile: false,
                                is_friend: false,
                            },
                        ),
                    };
                    let r = entry_of(class, &mut classes);
                    // 无标记 = public (见 [`parse_layout_ex`] 的同款约定)
                    r.access.push((key, info.access.unwrap_or(Access::Public)));
                    r.members.push(decl);
                } else {
                    let info = match parse_fn_signature_full(&e.signature) {
                        Some(v) => v,
                        None => continue,
                    };
                    let params: Vec<(String, MangleType)> = info
                        .params
                        .into_iter()
                        .enumerate()
                        .map(|(i, t)| (format!("__p{}", i), t))
                        .collect();
                    free.push(CppDecl::FreeFn {
                        ret: info.ret,
                        name: e.name.clone(),
                        params,
                        body: decl_body(),
                        is_noexcept: false,
                        trailing_ret_type: None,
                    });
                }
            },
            BmfKind::Variable => {
                if !seen.insert(format!("V {}", e.name)) {
                    continue;
                }
                if let Some(ty) = c_to_ty(&e.signature) {
                    free.push(CppDecl::GlobalVar {
                        ty,
                        name: e.name.clone(),
                        init: None,
                        is_static: false,
                        is_volatile: false,
                    });
                }
            },
            BmfKind::Template => {
                // M28.4: 模板体源文本回放 —— 交回 parser, 实例化管线无需修改
                let Some(body) = split_template_signature(&e.signature).1 else {
                    continue;
                };
                let Ok(decls) = CppParser::new(body.as_bytes(), "<bmf>").parse_top_level() else {
                    continue;
                };
                for mut d in decls {
                    if let CppDecl::Template { is_export, .. } = &mut d {
                        // BMF 里只存导出模板; 回放后重新收集才能保持接口不动点
                        *is_export = true;
                        free.push(d);
                    }
                }
            },
        }
    }
    let mut out: Vec<CppDecl> = Vec::new();
    for name in &order {
        let r = classes.remove(name).unwrap_or_else(|| Replay {
            members: Vec::new(),
            access: Vec::new(),
            is_struct: true,
            is_union: false,
            bases: Vec::new(),
        });
        out.push(CppDecl::Class(ClassDecl {
            name: name.clone(),
            is_struct: r.is_struct,
            is_union: r.is_union,
            bases: r.bases,
            members: r.members,
            member_access: r.access,
        }));
    }
    out.extend(free);
    out
}

/// `export` 声明是否属于模块**序言** (只能出现在 import 之后的那一段).
///
/// M53: 只把「无定义」的 export 当序言。`export int f() { ... }` / `export { ... }`
/// 里带函数体或类体的声明是普通用户声明, 导入声明必须注入在它**之前** ——
/// 否则函数体里的被调名字先于原型进入 sema, 报 `undeclared identifier`
/// (实测: `export module m; import :part; export int get(){ return pfn(); }`)。
fn export_block_is_preamble(d: &CppDecl) -> bool {
    match d {
        CppDecl::ExportBlock(inner) => inner.iter().all(export_block_is_preamble),
        // 函数/方法的纯声明形态 (`export int f();`) 无体, 算序言; 有体算用户声明.
        CppDecl::FreeFn { body, .. } | CppDecl::Ctor { body, .. } | CppDecl::Dtor { body, .. } => {
            body.is_decl
        },
        _ => true,
    }
}

/// 把导入模块的声明 splice 进用户 AST: 紧跟在开头的 `module` / `import`
/// 声明之后 (C++20 要求 import 先于其他声明, 注入声明再前置于用户声明).
pub fn merge_imported_decls(user: &[CppDecl], imported: &[BmfModule]) -> Vec<CppDecl> {
    let mut injected: Vec<CppDecl> = Vec::new();
    for m in imported {
        injected.extend(decls_from_bmf(m));
    }
    if injected.is_empty() {
        return user.to_vec();
    }
    let mut out: Vec<CppDecl> = Vec::with_capacity(user.len() + injected.len());
    let mut inserted = false;
    for d in user {
        let is_preamble = matches!(d, CppDecl::Module { .. } | CppDecl::Import { .. })
            || export_block_is_preamble(d);
        if !is_preamble && !inserted {
            out.extend(std::mem::take(&mut injected));
            inserted = true;
        }
        out.push(d.clone());
    }
    if !inserted {
        out.extend(injected);
    }
    out
}

// ===== M28.2 / M28.6: 发射符号名 (linkage) 与内部链接计划 =====

/// 本 TU 是否是模块 TU (含 `export module` / `module` / `import` / 私有片段).
///
/// M28.6 的改名仅对模块 TU 生效: 普通 C++ 源文件的符号约定保持不变,
/// 避免无必要的全局回归面.
pub fn is_module_tu(decls: &[CppDecl]) -> bool {
    decls.iter().any(|d| {
        matches!(
            d,
            CppDecl::Module { .. } | CppDecl::Import { .. } | CppDecl::PrivateFragment
        )
    })
}

/// 一个类在 native 产物里的全部「后端约定符号名」(M28.2/M28.6 共用).
fn class_symbol_names(c: &ClassDecl, out: &mut Vec<String>) {
    use crate::cpp::hir::{CtorVariant, DtorVariant};
    use crate::cpp::lower_ir::{ctor_variant_name, dtor_variant_name, member_func_symbol};
    for m in &c.members {
        match m {
            CppDecl::Ctor { class, .. } => {
                let cls = if class.is_empty() { c.name.clone() } else { class.clone() };
                for v in [CtorVariant::Complete, CtorVariant::Base, CtorVariant::Allocating] {
                    out.push(ctor_variant_name(&cls, v));
                }
            },
            CppDecl::Dtor { class, .. } => {
                let cls = if class.is_empty() { c.name.clone() } else { class.clone() };
                for v in [DtorVariant::Complete, DtorVariant::Base, DtorVariant::Deleting] {
                    out.push(dtor_variant_name(&cls, v));
                }
            },
            CppDecl::MemberFn { class, name, .. } => {
                let cls = if class.is_empty() { c.name.clone() } else { class.clone() };
                out.push(member_func_symbol(&cls, name));
            },
            _ => {},
        }
    }
}

/// 一条顶层声明的**源级实体名** (自由函数 / 全局变量 / 类 / 模板包的是哪个类或函数)
fn entity_source_name(d: &CppDecl) -> Option<String> {
    match d {
        CppDecl::FreeFn { name, .. } => Some(name.clone()),
        CppDecl::GlobalVar { name, .. } => Some(name.clone()),
        CppDecl::Class(c) => Some(c.name.clone()),
        CppDecl::Template { decl, .. } => entity_source_name(decl.as_ref()),
        _ => None,
    }
}

/// 接景区 (第一个 `module :private;` 之前) 被 **`export`** 标记的实体源名.
///
/// 标准里私有片段不能“反导出”: `export int use();` 已在接口里导出实体,
/// 把定义放进 `module :private;` 仍不改它的链接级与可见性 (M53: 单条声明的
/// `export` 在 parser 里就编码成 `ExportBlock([decl])`, 与 `export { decl }` 同形).
/// 而只写裸声明 (`int hidden_helper();`, 无 `export`) 的实体不记导出标记,
/// 依旧归私有.
fn exported_interface_names(decls: &[CppDecl]) -> std::collections::HashSet<String> {
    fn walk(inner: &[CppDecl], out: &mut std::collections::HashSet<String>) {
        for d in inner {
            match d {
                CppDecl::ExportBlock(nested) => walk(nested, out),
                CppDecl::Template { decl, is_export, .. } => {
                    if *is_export {
                        if let Some(n) = entity_source_name(decl.as_ref()) {
                            out.insert(n);
                        }
                    }
                },
                other => {
                    if let Some(n) = entity_source_name(other) {
                        out.insert(n);
                    }
                },
            }
        }
    }
    let mut out = std::collections::HashSet::new();
    for d in decls {
        match d {
            CppDecl::PrivateFragment => break,
            CppDecl::ExportBlock(inner) => walk(inner, &mut out),
            CppDecl::Template { decl, is_export, .. } if *is_export => {
                if let Some(n) = entity_source_name(decl.as_ref()) {
                    out.insert(n);
                }
            },
            _ => {},
        }
    }
    out
}

/// `module :private;` 之后、且**未**在接景区被 `export` 的声明 (M28.2 的真私有实体).
fn private_region_decls(decls: &[CppDecl]) -> Vec<&CppDecl> {
    let exported = exported_interface_names(decls);
    let mut priv_region = false;
    let mut out = Vec::new();
    for d in decls {
        match d {
            CppDecl::PrivateFragment => priv_region = true,
            _ if !priv_region => {},
            other => {
                if let Some(n) = entity_source_name(other) {
                    if exported.contains(&n) {
                        continue;
                    }
                }
                out.push(other);
            },
        }
    }
    out
}

/// M28.2: `module :private;` 之后声明的实体在 native 产物里的约定符号名集合.
///
/// 这些实体不进 BMF 接口, 对任何 TU 都不该是全局符号; 收集其自由函数 /
/// 全局变量 / 类成员 (ctor/dtor/成员函数按 `lower_ir` 的约定名), 交由
/// [`ModuleLinkagePlan::apply`] 降为内部链接 (STB_LOCAL).
pub fn private_fragment_symbols(decls: &[CppDecl]) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for d in private_region_decls(decls) {
        match d {
            CppDecl::FreeFn { name, .. } if name != "main" => {
                out.insert(name.clone());
            },
            CppDecl::GlobalVar { name, .. } => {
                out.insert(name.clone());
            },
            CppDecl::Class(c) => {
                let mut names = Vec::new();
                class_symbol_names(c, &mut names);
                // 类自身在私有片段 -> 其类型名也仅本 TU 可见, 但类型不占符号,
                // 只记成员函数符号; 同时记名以防未来按类名发射辅助符号
                out.extend(names);
            },
            CppDecl::Template { decl, .. } => {
                if let CppDecl::Class(c) = decl.as_ref() {
                    let mut names = Vec::new();
                    class_symbol_names(c, &mut names);
                    out.extend(names);
                }
            },
            _ => {},
        }
    }
    out
}

/// M28.5/M28.6: 把 ctor/dtor mangled 名的 ABI 变体数字换成 `digit`.
///
/// `_ZN6WidgetC1Ei` -> C2/C3 (基对象 / 分配版 ctor), `_ZN6WidgetD1Ev` -> D2/D0.
/// 逐层走 nested-name (含 `I...E` 模板参数) 后才取变体数字, 避免名字
/// 本体里出现 `C1E` 时被误改.
fn rekind_ctor_dtor(mangled: &str, digit: char) -> Option<String> {
    let rest = mangled.strip_prefix("_ZN")?;
    let b = rest.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'0'..=b'9' => {
                let mut n = 0usize;
                while i < b.len() && b[i].is_ascii_digit() {
                    n = n * 10 + (b[i] - b'0') as usize;
                    i += 1;
                }
                i += n;
            },
            b'I' => {
                let mut depth = 1i32;
                i += 1;
                while i < b.len() && depth > 0 {
                    match b[i] {
                        b'I' => depth += 1,
                        b'E' => depth -= 1,
                        _ => {},
                    }
                    i += 1;
                }
            },
            c @ (b'C' | b'D') if i + 1 < b.len() && b[i + 1].is_ascii_digit() => {
                let mut out = String::from("_ZN");
                out.push_str(&rest[..i]);
                out.push(c as char);
                out.push(digit);
                out.push_str(&rest[i + 2..]);
                return Some(out);
            },
            _ => return None,
        }
    }
    None
}

/// M28.6: native 产物 (`ast::Program`) 的符号链接计划.
#[derive(Debug, Default, Clone)]
pub struct ModuleLinkagePlan {
    /// 后端约定名 -> mangled 发射名
    pub rename: std::collections::HashMap<String, String>,
    /// 需降为内部链接 (STB_LOCAL) 的约定名
    pub internal: std::collections::HashSet<String>,
    /// 本 TU 不得发射的数据符号 (导入类的 `_ZTV`/`_ZTI`/`_ZTS`)
    pub drop_data: std::collections::HashSet<String>,
    /// M28.5 补: 本 TU 不得**定义**的函数约定名 (导入多态类的 ctor/dtor 变体).
    ///
    /// 导入端重放的是纯声明, native 管线会为它再合成一份隐式 ctor/dtor, 但那份
    /// 副本**不写 vptr** (虚表已按 Itanium 规矩从本 TU 剔除, 见 [`Self::drop_data`]),
    /// 与导出 TU 的 weak 定义**内容不一致**. weak 符号链接期任选其一: 选中本地
    /// 副本时, 在导入 TU 里构造的对象 vptr 未初始化 -> 虚调用段错 (且只在
    /// “导入 TU 排在前”时出现, 链接顺序敏感). 根因修法: 本 TU 不定义,
    /// 调用解析到导出 TU 的真定义 (与用户定义 ctor `Rect(int,int)` 的处理一致).
    pub externalize: std::collections::HashSet<String>,
}

/// M28.5 补: 模块图 (own + 全部 imported) 里**多态** (自有或继承虚函数) 的类名.
///
/// 必须跨模块汇总: `shape` 导出多态基类 `Shape`, `shape2` 导出 `Rect : public Shape`
/// 且只重写声明 (`int area();`, 未写 `virtual`) —— 只看 `shape2` 自己的导出无法
/// 判出 `Rect` 也是多态类. 模板类除外: 模板在导入端本地实例化 (`@body=`),
/// 其 ctor/dtor 与虚表属于本 TU, 不得外部化.
fn polymorphic_classes(mods: &[&BmfModule]) -> std::collections::HashSet<String> {
    use std::collections::HashSet;
    let mut poly: HashSet<String> = HashSet::new();
    let mut parents: Vec<(String, String)> = Vec::new();
    let mut templates: HashSet<String> = HashSet::new();
    for m in mods {
        for e in &m.exports {
            match e.kind {
                BmfKind::Template => {
                    templates.insert(e.name.clone());
                },
                BmfKind::Function => {
                    if e.signature.contains(VIRTUAL_MARK) {
                        if let Some((class, _)) = e.name.split_once("::") {
                            poly.insert(class.to_string());
                        }
                    }
                },
                BmfKind::Class => {
                    let layout = parse_layout_ex(e.layout.as_deref().unwrap_or(""));
                    for b in &layout.bases {
                        parents.push((e.name.clone(), b.name.clone()));
                    }
                },
                BmfKind::Variable => {},
            }
        }
    }
    // 基类多态 -> 派生类多态 (不动点传接; 类图很小)
    loop {
        let mut add: Vec<String> = Vec::new();
        for (d, b) in &parents {
            if poly.contains(b) && !poly.contains(d) {
                add.push(d.clone());
            }
        }
        if add.is_empty() {
            break;
        }
        poly.extend(add);
    }
    for t in templates {
        poly.remove(&t);
    }
    poly
}

/// M28.5 补: 导入的多态类在导入 TU 里不得定义的 ctor/dtor 约定名.
///
/// 只覆盖 `complete` / `base` 两个变体 —— `allocating`(C3) / `deleting`(D0)
/// 带 `operator new`/`delete` 配合, 由**使用侧**自行发射 (导出 TU 不
/// 一定会发), 外部化会造成未定义符号.
fn imported_ctor_dtor_symbols(
    imported: &[BmfModule],
    poly: &std::collections::HashSet<String>,
    local_classes: &[String],
) -> Vec<String> {
    use crate::cpp::hir::{CtorVariant, DtorVariant};
    use crate::cpp::lower_ir::{ctor_variant_name, dtor_variant_name};
    let mut out = Vec::new();
    for m in imported {
        for e in &m.exports {
            if e.kind != BmfKind::Class {
                continue;
            }
            if !poly.contains(&e.name) || local_classes.contains(&e.name) {
                continue;
            }
            for v in [CtorVariant::Complete, CtorVariant::Base] {
                out.push(ctor_variant_name(&e.name, v));
            }
            for v in [DtorVariant::Complete, DtorVariant::Base] {
                out.push(dtor_variant_name(&e.name, v));
            }
        }
    }
    out
}

/// M28.5: 导入类的 RTTI/虚表符号 —— 发射侧必须交给导出 TU.
///
/// 导入端重放的类是**纯声明** (无函数体), native 后端会为它再发一份
/// `_ZTV<C>`/`_ZTI<C>`/`_ZTS<C>` weak 对象, 而那份虚表的功能槽全为 0;
/// 链接期与导出 TU 的那份同名 weak 符号随机二选一 -> 虚调用打到空槽段错.
/// Itanium 的规矩是虚表由 key function 所在 TU 唯一发射, 故导入 TU 直接
/// 不发射, 引用解析到导出 TU 的定义.
fn imported_rtti_symbols(m: &BmfModule, local_classes: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for e in &m.exports {
        if e.kind != BmfKind::Class || local_classes.contains(&e.name) {
            continue;
        }
        out.push(mangle_vtable(&e.name));
        out.push(mangle_typeinfo(&e.name));
        out.push(format!("_ZTS{}", encode_type_name(&e.name)));
    }
    out
}

/// 本 TU 自己定义的类名 (含 `export {}` / 模板体内的类)
fn local_class_names(decls: &[CppDecl]) -> Vec<String> {
    let mut classes = Vec::new();
    gather_classes(decls, &mut classes);
    classes.into_iter().map(|c| c.name.clone()).collect()
}

impl ModuleLinkagePlan {
    pub fn is_empty(&self) -> bool {
        self.rename.is_empty()
            && self.internal.is_empty()
            && self.drop_data.is_empty()
            && self.externalize.is_empty()
    }

    /// 把计划施加到 native 产物上: 导出/导入实体统一改名为 mangled 符号,
    /// 私有片段实体降为内部链接.
    ///
    /// 为什么在 `ast::Program` 这一层做: 定义侧与所有调用点 (含 vtable 槽的
    /// `&sym`) 在这一层都是同一个名字, 一次统一改名不会留下“一半旧名”的
    /// 引用; 而符号名的唯一真源 `lower_ir::symbol_link_name` 属于在途修改区,
    /// 不适合由模块侧擅改.
    pub fn apply(&self, prog: &mut crate::ast::Program) {
        if self.is_empty() {
            return;
        }
        // M28.5 补: 导入多态类的 ctor/dtor 不在本 TU 定义 (改名前按约定名删,
        // 那些名字尚未进入 rename 表)
        if !self.externalize.is_empty() {
            let ext = &self.externalize;
            prog.decls.retain(|d| match d {
                crate::ast::Decl::Func(f) => !ext.contains(&f.name.name),
                _ => true,
            });
        }
        // 安全阀: 枚举常量名 / 布告栏名不参与改名 (它们的声明不在改名集里,
        // 改了引用会导致 ir::lower 找不到名字)
        let mut blocked: std::collections::HashSet<String> = std::collections::HashSet::new();
        for d in &prog.decls {
            if let crate::ast::Decl::EnumDef(e) = d {
                for (v, _) in &e.variants {
                    blocked.insert(v.name.clone());
                }
            }
        }
        for d in &mut prog.decls {
            match d {
                crate::ast::Decl::Var(v) => {
                    if self.internal.contains(&v.name.name) {
                        v.is_static = true;
                        v.is_weak = false;
                    }
                    if let Some(m) = self.rename.get(&v.name.name) {
                        v.name.name = m.clone();
                    }
                    if let Some(e) = &mut v.init {
                        self.ren_expr(e, &blocked);
                    }
                },
                crate::ast::Decl::Func(f) => {
                    if self.internal.contains(&f.name.name) {
                        // STB_LOCAL 优先级在 codegen 里低于 weak, 需清除 weak 标记
                        f.is_static = true;
                        f.is_weak = false;
                    }
                    for p in &mut f.params {
                        if let Some(id) = &mut p.name {
                            self.ren_ident(id, &blocked);
                        }
                    }
                    if let Some(b) = &mut f.body {
                        self.ren_block(b, &blocked);
                    }
                    if let Some(m) = self.rename.get(&f.name.name) {
                        f.name.name = m.clone();
                    }
                },
                // StructDef / UnionDef / EnumDef / Typedef / GlobalAsm:
                // 类型名与字段名不是链接符号, 不改名
                _ => {},
            }
        }
        // M28.5: 导入类的虚表/typeinfo 由导出 TU 发射, 本 TU 删掉重放的空壳
        if !self.drop_data.is_empty() {
            let drop = &self.drop_data;
            prog.decls.retain(|d| match d {
                crate::ast::Decl::Var(v) => !drop.contains(&v.name.name),
                _ => true,
            });
        }
    }

    fn ren_ident(&self, id: &mut crate::ast::Ident, blocked: &std::collections::HashSet<String>) {
        if blocked.contains(&id.name) {
            return;
        }
        if let Some(m) = self.rename.get(&id.name) {
            id.name = m.clone();
        }
    }

    fn ren_opt(&self, e: &mut Option<crate::ast::Expr>, blocked: &std::collections::HashSet<String>) {
        if let Some(e) = e {
            self.ren_expr(e, blocked);
        }
    }

    fn ren_inits(
        &self,
        inits: &mut Vec<crate::ast::InitItem>,
        blocked: &std::collections::HashSet<String>,
    ) {
        use crate::ast::{Designator, InitItem};
        for it in inits {
            match it {
                InitItem::Expr(e) => self.ren_expr(e, blocked),
                InitItem::Designated { designator, value } => {
                    self.ren_designator(designator, blocked);
                    self.ren_expr(value, blocked);
                },
            }
        }
    }

    /// 指定符: `.field` 是字段名 (不改), `[idx]` / `{a...b}` 是表达式 (改)
    fn ren_designator(
        &self,
        d: &mut crate::ast::Designator,
        blocked: &std::collections::HashSet<String>,
    ) {
        use crate::ast::Designator;
        match d {
            Designator::Field(_) => {},
            Designator::Index(e) => self.ren_expr(e, blocked),
            Designator::Range(a, b) => {
                self.ren_expr(a, blocked);
                self.ren_expr(b, blocked);
            },
            Designator::Chain(v) => {
                for x in v {
                    self.ren_designator(x, blocked);
                }
            },
        }
    }

    fn ren_expr(
        &self,
        e: &mut crate::ast::Expr,
        blocked: &std::collections::HashSet<String>,
    ) {
        use crate::ast::Expr as E;
        match e {
            E::Ident { name, .. } => self.ren_ident(name, blocked),
            E::Real { e, .. }
            | E::Imag { e, .. }
            | E::Unary { e, .. }
            | E::Cast { e, .. }
            | E::SizeofExpr { e, .. }
            | E::AddrOf { e, .. }
            | E::Deref { e, .. }
            | E::PostInc { e, .. }
            | E::PostDec { e, .. }
            | E::PreInc { e, .. }
            | E::PreDec { e, .. }
            | E::AlignofExpr { e, .. } => self.ren_expr(e, blocked),
            E::Binop { l, r, .. }
            | E::Assign { l, r, .. }
            | E::Comma { l, r, .. } => {
                self.ren_expr(l, blocked);
                self.ren_expr(r, blocked);
            },
            E::Index { arr, idx, .. } => {
                self.ren_expr(arr, blocked);
                self.ren_expr(idx, blocked);
            },
            E::Call { callee, args, .. } => {
                self.ren_expr(callee, blocked);
                for a in args {
                    self.ren_expr(a, blocked);
                }
            },
            E::Cond { cond, then_, else_, .. } => {
                self.ren_expr(cond, blocked);
                self.ren_expr(then_, blocked);
                self.ren_expr(else_, blocked);
            },
            // `Member{expr, field}`: field 是字段名, 不参与符号改名
            E::Member { expr, .. } => self.ren_expr(expr, blocked),
            E::CompoundLit { inits, .. } | E::InitList { inits, .. } => {
                self.ren_inits(inits, blocked)
            },
            E::StmtExpr { block, .. } => self.ren_block(block, blocked),
            E::AsmExpr { outputs, inputs, .. } => {
                for x in outputs.iter_mut().chain(inputs.iter_mut()) {
                    self.ren_expr(x, blocked);
                }
            },
            E::VaArg { ap, .. } => self.ren_expr(ap, blocked),
            E::Offsetof { indices, .. } => {
                for x in indices {
                    self.ren_expr(x, blocked);
                }
            },
            // Int / Float / ComplexLit / ComplexIntLit / String / Char / SizeofTy
            // / AlignofTy / LabelAddr / Designated: 不携带独立标识符引用
            _ => {},
        }
    }

    fn ren_block(
        &self,
        b: &mut crate::ast::Block,
        blocked: &std::collections::HashSet<String>,
    ) {
        use crate::ast::{BlockItem, Decl};
        for it in &mut b.items {
            match it {
                BlockItem::Stmt(s) => self.ren_stmt(s, blocked),
                BlockItem::Decl(Decl::Var(v)) => {
                    if self.internal.contains(&v.name.name) {
                        v.is_static = true;
                        v.is_weak = false;
                    }
                    // 局部/参数声明与它们的引用一起改名, 保证作图一致
                    if let Some(m) = self.rename.get(&v.name.name) {
                        v.name.name = m.clone();
                    }
                    if let Some(e) = &mut v.init {
                        self.ren_expr(e, blocked);
                    }
                },
                BlockItem::Decl(Decl::Func(f)) => {
                    if self.internal.contains(&f.name.name) {
                        f.is_static = true;
                        f.is_weak = false;
                    }
                    for p in &mut f.params {
                        if let Some(id) = &mut p.name {
                            self.ren_ident(id, blocked);
                        }
                    }
                    if let Some(body) = &mut f.body {
                        self.ren_block(body, blocked);
                    }
                    if let Some(m) = self.rename.get(&f.name.name) {
                        f.name.name = m.clone();
                    }
                },
                BlockItem::Decl(_) => {},
            }
        }
    }

    fn ren_stmt(
        &self,
        s: &mut crate::ast::Stmt,
        blocked: &std::collections::HashSet<String>,
    ) {
        use crate::ast::Stmt;
        match s {
            Stmt::Return { val, .. } => self.ren_opt(val, blocked),
            Stmt::Expr { e, .. } => self.ren_expr(e, blocked),
            Stmt::If { cond, then_, else_, .. } => {
                self.ren_expr(cond, blocked);
                self.ren_stmt(then_, blocked);
                if let Some(x) = else_ {
                    self.ren_stmt(x, blocked);
                }
            },
            Stmt::While { cond, body, .. } => {
                self.ren_expr(cond, blocked);
                self.ren_stmt(body, blocked);
            },
            Stmt::Do { body, cond, .. } => {
                self.ren_stmt(body, blocked);
                self.ren_expr(cond, blocked);
            },
            Stmt::For { init, cond, step, body, .. } => {
                if let Some(i) = init {
                    self.ren_stmt(i, blocked);
                }
                self.ren_opt(cond, blocked);
                self.ren_opt(step, blocked);
                self.ren_stmt(body, blocked);
            },
            Stmt::Block { b, .. } => self.ren_block(b, blocked),
            Stmt::Switch { expr, cases, .. } => {
                self.ren_expr(expr, blocked);
                for c in cases {
                    self.ren_opt(&mut c.val, blocked);
                    self.ren_opt(&mut c.range_end, blocked);
                    self.ren_block(&mut c.body, blocked);
                }
            },
            Stmt::Label { stmt, .. } => self.ren_stmt(stmt, blocked),
            Stmt::GotoIndirect { expr, .. } => self.ren_expr(expr, blocked),
            Stmt::AsmGoto { outputs, inputs, .. } => {
                for x in outputs.iter_mut().chain(inputs.iter_mut()) {
                    self.ren_expr(x, blocked);
                }
            },
            // Break / Continue / Goto(标签名) / Fallthrough: 无表达式引用
            _ => {},
        }
    }
}

/// 一条 BMF 导出记录 -> 「后端约定名 -> mangled 发射名」条目.
fn add_export_renames(
    map: &mut std::collections::HashMap<String, String>,
    e: &BmfExport,
) {
    use crate::cpp::hir::{CtorVariant, DtorVariant};
    use crate::cpp::lower_ir::{ctor_variant_name, dtor_variant_name, member_func_symbol};
    match e.kind {
        BmfKind::Function => {
            let Some(mg) = signature_mangled(&e.signature) else { return };
            match e.name.split_once("::") {
                Some((class, "ctor")) => {
                    map.insert(ctor_variant_name(class, CtorVariant::Complete), mg.clone());
                    if let Some(b) = rekind_ctor_dtor(&mg, '2') {
                        map.insert(ctor_variant_name(class, CtorVariant::Base), b);
                    }
                    if let Some(a) = rekind_ctor_dtor(&mg, '3') {
                        map.insert(ctor_variant_name(class, CtorVariant::Allocating), a);
                    }
                },
                Some((class, "dtor")) => {
                    map.insert(dtor_variant_name(class, DtorVariant::Complete), mg.clone());
                    if let Some(b) = rekind_ctor_dtor(&mg, '2') {
                        map.insert(dtor_variant_name(class, DtorVariant::Base), b);
                    }
                    if let Some(d) = rekind_ctor_dtor(&mg, '0') {
                        map.insert(dtor_variant_name(class, DtorVariant::Deleting), d);
                    }
                },
                Some((class, meth)) => {
                    map.insert(member_func_symbol(class, meth), mg);
                },
                None => {
                    map.insert(e.name.clone(), mg);
                },
            }
        },
        BmfKind::Variable => {
            map.insert(e.name.clone(), mangle_global_variable(&e.name));
        },
        // 类 / 模板不占独立发射符号 (模板体在导入端重新实例化)
        BmfKind::Class | BmfKind::Template => {},
    }
}

/// M28.6: 推导本 TU 的链接名计划.
///
/// - 改名表来源: 本 TU 导出 (`own_interface`) + 全部导入模块的 `E F` / `E V`
///   记录里的 mangled 名 (后端约定名 -> mangled 发射名)
/// - 内部链接集: `module :private;` 片段实体
///
/// 非模块 TU 返回空计划 (零影响).
pub fn module_linkage_plan(
    user_decls: &[CppDecl],
    own: Option<&BmfModule>,
    imported: &[BmfModule],
) -> ModuleLinkagePlan {
    let mut plan = ModuleLinkagePlan::default();
    if !is_module_tu(user_decls) {
        return plan;
    }
    let mut mods: Vec<&BmfModule> = Vec::new();
    if let Some(m) = own {
        mods.push(m);
    }
    mods.extend(imported.iter());
    for m in &mods {
        for e in &m.exports {
            add_export_renames(&mut plan.rename, e);
        }
    }
    plan.internal = private_fragment_symbols(user_decls);
    // M28.5: 导入 (非本地定义) 的类不得在本 TU 发射虚表/typeinfo
    let local_classes = local_class_names(user_decls);
    for m in imported {
        plan.drop_data.extend(imported_rtti_symbols(m, &local_classes));
    }
    // M28.5 补: 多态性先按整张模块图 (own + imported) 不动点传接, 再把导入
    // 多态类的 ctor/dtor 外部化 (否则得到一份不写 vptr 的 weak 副本,
    // 链接顺序敏感地盖掉导出 TU 的真定义 -> 虚调用段错)
    let poly = polymorphic_classes(&mods);
    plan.externalize.extend(imported_ctor_dtor_symbols(imported, &poly, &local_classes));
    plan
}

/// M26.5: ODR 一致性检查. 把本 TU 的导出与全部导入模块的导出注册进
/// [`OdrChecker`], 返回违规描述文本 (None = 无违规).
///
/// 判定: 同名实体的签名 (函数/变量) 或类布局文本必须一致; 一致则视为
/// 同一实体的重复声明 (合法), 不一致即 ODR 违规.
pub fn odr_conflicts(own: Option<&BmfModule>, imported: &[BmfModule]) -> Option<String> {
    let mut checker = OdrChecker::new();
    if let Some(m) = own {
        if !m.name.is_empty() && !m.exports.is_empty() {
            checker.register_module(&m.name, &m.exports);
        }
    }
    for m in imported {
        checker.register_module(&m.name, &m.exports);
    }
    checker.check_and_format()
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
    ///
    /// M28.6: 比较键改为实体的**链接身份** (函数/变量 = mangled 名, 类 = 类型名,
    /// 模板 = 模板名), 所以 `foo()` 与 `foo(int)` 不再撞键 —— 它们是两个不同的
    /// 合法实体. 同键但签名/布局文本不同 = 真实的 ODR 违例.
    pub fn register_module(&mut self, module_name: &str, exports: &[BmfExport]) {
        for e in exports {
            let (key, sig) = odr_key(e);
            self.definitions.entry(key).or_default().push((module_name.to_string(), sig));
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

/// M28.6: ODR 比较键 -> (键, 用于一致性比较的定义文本).
///
/// - 函数: mangled 名 (已含重载参数); 无 mangled 字段时回退到 `Class::meth`/源名
/// - 变量: mangled 变量名
/// - 类: 类型名 (布局文本作比较值 -> 同名不同布局 = 违例)
/// - 模板: 模板名 (签名含 `@body=` -> 同名不同定义体 = 违例)
pub fn odr_key(e: &BmfExport) -> (String, String) {
    match e.kind {
        BmfKind::Function => {
            let key = match signature_mangled(&e.signature) {
                Some(m) => format!("F {}", m),
                None => format!("F {}", e.name),
            };
            (key, e.signature.clone())
        },
        BmfKind::Variable => (format!("V {}", mangle_global_variable(&e.name)), e.signature.clone()),
        BmfKind::Class => (format!("C {}", e.name), e.layout.clone().unwrap_or_default()),
        BmfKind::Template => (format!("T {}", e.name), e.signature.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpp::bmf::{BmfExport, BmfKind};
    use crate::cpp::parser::CppParser;

    /// 解析一小段 C++ 源码为顶层声明
    fn parse_decls(src: &str) -> Vec<CppDecl> {
        let mut p = CppParser::new(src.as_bytes(), "unit.cpp");
        p.parse_top_level().expect("parse module source")
    }

    const EXPORT_TU: &str = "export module mymod;\nexport int answer() { return 42; }\nexport int twice(int x) { return x * 2; }\nexport class Widget {\npublic:\n    int id;\n    Widget(int v) { id = v; }\n    int get() { return id; }\n};\nmodule :private;\nint hidden_helper() { return 1; }\n";

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
        // 模块 B 导出 foo(double) - 同键但不同签名, ODR 违规
        let exports_b = vec![BmfExport {
            kind: BmfKind::Function,
            name: "foo".to_string(),
            signature: "int(double)".to_string(),
            layout: None,
        }];
        checker.register_module("B", &exports_b);
        let violations = checker.check();
        assert_eq!(violations.len(), 1);
        // M28.6: 比较键 = 种类前缀 + 链接身份 (无 mangled 名时回退到源名)
        assert_eq!(violations[0].0, "F foo");
        let msg = checker.check_and_format().unwrap();
        assert!(msg.contains("ODR violations"));
        assert!(msg.contains("foo"));
        assert!(msg.contains("module 'A'"));
        assert!(msg.contains("module 'B'"));
    }

    #[test]
    fn odr_key_uses_mangled_link_identity() {
        // M28.6: 函数按 mangled 名撞键 —— `foo()` 与 `foo(int)` 是两个不同
        // 的合法实体 (重载), 不再互相判为 ODR 冲突
        let f0 = BmfExport {
            kind: BmfKind::Function,
            name: "foo".to_string(),
            signature: "int _Z3foov()".to_string(),
            layout: None,
        };
        let f1 = BmfExport {
            kind: BmfKind::Function,
            name: "foo".to_string(),
            signature: "int _Z3fooi(int)".to_string(),
            layout: None,
        };
        assert_eq!(odr_key(&f0).0, "F _Z3foov");
        assert_eq!(odr_key(&f1).0, "F _Z3fooi");
        let mod_of = |name: &str, e: &BmfExport| BmfModule {
            name: name.to_string(),
            exports: vec![e.clone()],
            ..Default::default()
        };
        assert!(odr_conflicts(None, &[mod_of("a", &f0), mod_of("b", &f1)]).is_none());
        // 同 mangled 名但类型文本不同 (Itanium 不编返回类型) = 真实违例
        let f0_bad = BmfExport { signature: "long _Z3foov()".to_string(), ..f0.clone() };
        let msg =
            odr_conflicts(None, &[mod_of("a", &f0), mod_of("b", &f0_bad)]).expect("same mangled");
        assert!(msg.contains("_Z3foov"), "{}", msg);
        // 变量按 mangled 变量名, 类按类型名, 模板按模板名
        let v = BmfExport {
            kind: BmfKind::Variable,
            name: "g_count".to_string(),
            signature: "int".to_string(),
            layout: None,
        };
        assert_eq!(odr_key(&v).0, "V _Z7g_count");
        let c = BmfExport {
            kind: BmfKind::Class,
            name: "Point".to_string(),
            signature: String::new(),
            layout: Some("@struct int x;".to_string()),
        };
        assert_eq!(odr_key(&c), ("C Point".to_string(), "@struct int x;".to_string()));
        let t = BmfExport {
            kind: BmfKind::Template,
            name: "Box".to_string(),
            signature: "T".to_string(),
            layout: None,
        };
        assert_eq!(odr_key(&t).0, "T Box");
    }

    // ===== M26.4/M26.5: native 路径接口管道单元测试 =====

    #[test]
    fn collect_interface_exports_functions_classes_and_members() {
        let bmf = collect_interface(&parse_decls(EXPORT_TU)).expect("export module -> interface");
        assert_eq!(bmf.name, "mymod");
        assert_eq!(bmf.partition, None);
        let text = bmf.serialize();
        // 自由函数 / 类布局 / ctor + 成员函数均入接口
        assert!(text.contains("E F answer int "), "{}", text);
        assert!(text.contains("E F twice int "), "{}", text);
        // M28.3/M28.5: `E C` 布局首 token 是 class/struct/union 标记;
        // public 成员不写访问标记 (故旧文本仍可读)
        assert!(text.contains("E C Widget @class int id;"), "{}", text);
        assert!(text.contains("E F Widget::ctor "), "{}", text);
        assert!(text.contains("E F Widget::get "), "{}", text);
        // M28.6: 发射名走 mangled (`E F` 签名里带 `_Z...`)
        assert!(text.contains("_ZN6Widget3getEv"), "{}", text);
        // 私有片段不导出
        assert!(!text.contains("hidden_helper"), "{}", text);
        // 导出的签名必须可被 bmf 解析器回放 (行格式合法)
        let back = BmfModule::parse(&text).expect("parse back");
        assert_eq!(back, bmf);
    }

    #[test]
    fn private_fragment_entity_with_purview_declaration_stays_out_of_interface() {
        // M28.2: 私有片段实体即使在 purview 里有向前声明 (C++ 里要跨片段使用
        // 就必须先声明), 也不得进入 BMF 接口: 否则导入端会重放出一个指向
        // STB_LOCAL 定义的 extern, 链接期报 undefined reference.
        let src = "export module pfmod;\nint hidden_helper();\n\
                   export int visible() { return hidden_helper() + 1; }\n\
                   module :private;\nint hidden_helper() { return 41; }\n";
        let decls = parse_decls(src);
        let bmf = collect_interface(&decls).expect("interface");
        let text = bmf.serialize();
        assert!(text.contains("E F visible"), "{}", text);
        assert!(!text.contains("hidden_helper"), "私有片段实体泄漏进接口: {}", text);
        // 链接计划仍负责把其定义降为内部链接 (nm 侧断言见集成测试)
        let plan = module_linkage_plan(&decls, Some(&bmf), &[]);
        assert!(plan.internal.contains("hidden_helper"), "{:?}", plan.internal);
        assert!(!plan.rename.contains_key("hidden_helper"), "{:?}", plan.rename);
    }

    #[test]
    fn export_block_declaration_defined_in_private_fragment_stays_exported() {
        // M28.2: 标准里私有片段不能“反导出”. 用 `export { ... }` 块拼写时
        // 导出标记可被识别 -> 实体仍在接口里, 不得降为内部链接;
        // 否则导入端会重放出一个指向 STB_LOCAL 定义的引用.
        let src = "export module pfblock;\nexport {\nint use();\n}\n\
                   module :private;\nint use() { return 7; }\n";
        let decls = parse_decls(src);
        let bmf = collect_interface(&decls).expect("interface");
        let text = bmf.serialize();
        assert!(text.contains("E F use"), "`export {{}}` 里声明的实体仍导出: {}", text);
        let plan = module_linkage_plan(&decls, Some(&bmf), &[]);
        assert!(!plan.internal.contains("use"), "导出不该被降为内部链接: {:?}", plan.internal);
        assert!(plan.rename.contains_key("use"), "导出实体要有 mangled 发射名: {:?}", plan.rename);
    }

    #[test]
    fn export_single_declaration_defined_in_private_fragment_stays_exported() {
        // M53 任务 6: 上面那个测试的**裸声明**拼写镜像. [module.global.frag]
        // 里 `export decl` 与 `export { decl }` 等价, 两者必须得到同样的接口
        // 记录与链接级. 旧行为: parser 透传丢弃 `export` 关键字 ->
        // `exported_interface_names` 看不见它 -> 实体被误当真私有降为
        // STB_LOCAL, 导入端重放出指向内部链接定义的 undefined reference.
        let src = "export module pfsingle;\nexport int use();\n\
                   module :private;\nint use() { return 7; }\n";
        let decls = parse_decls(src);
        let bmf = collect_interface(&decls).expect("interface");
        let text = bmf.serialize();
        assert!(
            text.contains("E F use"),
            "`export` 修饰的单条声明仍要进接口: {}",
            text
        );
        let plan = module_linkage_plan(&decls, Some(&bmf), &[]);
        assert!(
            !plan.internal.contains("use"),
            "导出声明不该被降为内部链接: {:?}",
            plan.internal
        );
        assert!(
            plan.rename.contains_key("use"),
            "导出实体要有 mangled 发射名: {:?}",
            plan.rename
        );
    }

    #[test]
    fn imported_polymorphic_class_ctor_dtor_is_externalized() {
        // M28.5 补: 导入 TU 不得定义导入**多态**类的隐式 ctor/dtor (重放副本不写
        // vptr, 与导出 TU 的 weak 定义同名不同内容 -> 链接顺序敏感地段错).
        // 非多态类 / 模板类 / 本 TU 自定的子类不受影响.
        let mod_src = "export module shape;\nexport class Shape { public: virtual int area() { return 0; } };\nexport struct Point { int x; };\nexport template<class T> struct Box { T v; };\n";
        let decls = parse_decls(mod_src);
        let bmf = collect_interface(&decls).expect("interface");
        // 导入 TU: 只 `import`, 不本地定义任何类
        let use_decls = parse_decls("module user;\nimport shape;\nint f() { return 0; }\n");
        let plan = module_linkage_plan(&use_decls, None, std::slice::from_ref(&bmf));
        assert!(plan.externalize.contains("Shape_ctor_complete"), "{:?}", plan.externalize);
        assert!(plan.externalize.contains("Shape_dtor_base"), "{:?}", plan.externalize);
        // 多态性传接: 从 Shape 导出的类也要外部化
        let derived_src =
            "export module shape2;\nimport shape;\nexport class Rect : public Shape { public: int area(); };\n";
        let ddecls = parse_decls(derived_src);
        let dbmf = collect_interface(&ddecls).expect("derived interface");
        let poly = polymorphic_classes(&[&bmf, &dbmf]);
        assert!(poly.contains("Shape"), "{:?}", poly);
        assert!(poly.contains("Rect"), "基类虚函数应传接: {:?}", poly);
        assert!(!poly.contains("Box"), "模板类本地实例化: {:?}", poly);
        // 跨模块图外部化: 导入 shape + shape2 的 TU 两个类都不得定义 ctor
        let use2 = parse_decls("module user2;\nimport shape;\nimport shape2;\nint g() { return 0; }\n");
        let plan2 = module_linkage_plan(&use2, None, &[bmf.clone(), dbmf.clone()]);
        assert!(plan2.externalize.contains("Rect_ctor_complete"), "{:?}", plan2.externalize);
        assert!(plan2.externalize.contains("Shape_dtor_complete"), "{:?}", plan2.externalize);
        // 非多态类与模板类不在集合里
        assert!(!plan.externalize.iter().any(|n| n.starts_with("Point_")), "{:?}", plan.externalize);
        assert!(!plan.externalize.iter().any(|n| n.starts_with("Box_")), "{:?}", plan.externalize);
        // allocating(C3) / deleting(D0) 由使用侧发射, 不得外部化
        assert!(!plan.externalize.contains("Shape_ctor_allocating"));
        assert!(!plan.externalize.contains("Shape_dtor_deleting"));
    }

    #[test]
    fn collect_interface_skips_implementation_units() {
        // 无 `export module` 的 TU (实现单元 / 普通源文件) 不产出接口
        assert!(collect_interface(&parse_decls("int f() { return 1; }\n")).is_none());
        // `module m;` (is_export=false) 也不产出
        let d = vec![
            CppDecl::Module { name: Some("m".to_string()), partition: None, is_export: false },
            parse_decls("int f() { return 1; }")[0].clone(),
        ];
        let iface = collect_interface(&d).expect("named module yields interface record");
        assert!(iface.exports.is_empty(), "非导出模块不应收集接口: {:?}", iface.exports);
    }

    #[test]
    fn imported_decls_replay_to_identical_interface() {
        // BMF -> CppDecl -> BMF 不动点: 导入端重建的声明与原接口记录集一致
        // (记录顺序允许归一化: 类先于自由实体, 以满足名字查找顺序)
        let bmf = collect_interface(&parse_decls(EXPORT_TU)).unwrap();
        let mut decls = vec![CppDecl::Module {
            name: Some("mymod".to_string()),
            partition: None,
            is_export: true,
        }];
        decls.extend(decls_from_bmf(&bmf));
        let again = collect_interface(&decls).expect("re-collect");
        assert_eq!(again.name, bmf.name);
        let key = |e: &BmfExport| {
            (
                format!("{:?}", e.kind),
                e.name.clone(),
                e.signature.clone(),
                e.layout.clone(),
            )
        };
        let mut a: Vec<_> = bmf.exports.iter().map(key).collect();
        let mut b: Vec<_> = again.exports.iter().map(key).collect();
        a.sort();
        b.sort();
        assert_eq!(a, b, "接口回放不保真");
    }

    #[test]
    fn decls_from_bmf_builds_decl_only_class_and_functions() {
        let bmf = collect_interface(&parse_decls(EXPORT_TU)).unwrap();
        let injected = decls_from_bmf(&bmf);
        let mut n_class = 0;
        let mut n_fn = 0;
        for d in &injected {
            match d {
                CppDecl::Class(c) => {
                    n_class += 1;
                    assert_eq!(c.name, "Widget");
                    // 字段 + ctor + 成员函数, 全部为纯声明
                    assert!(c.members.iter().any(|m| matches!(m, CppDecl::MemberVar { name, .. } if name == "id")));
                    assert!(c.members.iter().any(|m| matches!(m, CppDecl::Ctor { body, .. } if body.is_decl)));
                    assert!(c.members.iter().any(
                        |m| matches!(m, CppDecl::MemberFn { name, body, .. } if name == "get" && body.is_decl)
                    ));
                },
                CppDecl::FreeFn { body, .. } => {
                    n_fn += 1;
                    assert!(body.is_decl, "导入的自由函数只能声明, 定义在导出 TU");
                },
                _ => {},
            }
        }
        assert_eq!(n_class, 1);
        assert_eq!(n_fn, 2);
    }

    #[test]
    fn imported_decls_are_injected_after_module_preamble() {
        let user = parse_decls("import mymod;\nint main() { return answer(); }\n");
        let bmf = collect_interface(&parse_decls(EXPORT_TU)).unwrap();
        let merged = merge_imported_decls(&user, &[bmf]);
        assert!(merged.len() > user.len());
        assert!(matches!(&merged[0], CppDecl::Import { path } if path == &["mymod".to_string()]));
        // 紧接 import 之后是注入声明, 用户 main 仍在末尾
        assert!(matches!(&merged[1], CppDecl::Class(c) if c.name == "Widget"));
        assert!(matches!(merged.last(), Some(CppDecl::FreeFn { name, .. }) if name == "main"));
    }

    #[test]
    fn imported_decls_are_injected_before_exported_definition() {
        // M53: 带定义的 `export int get() { ... }` 不是模块序言 (parser 把它编码成
        // `ExportBlock([FreeFn])`, 与 `export { int get(); }` 同形). 导入声明必须
        // 注入在它**之前**, 否则函数体里的 `answer()` 先于原型进入 sema 会报
        // undeclared identifier.
        let user = parse_decls("export module mymod;\nimport :part;\nexport int get() { return answer(); }\n");
        let bmf = collect_interface(&parse_decls(EXPORT_TU)).expect("interface");
        let merged = merge_imported_decls(&user, &[bmf]);
        let injected = merged
            .iter()
            .position(|d| matches!(d, CppDecl::FreeFn { name, body, .. } if name == "answer" && body.is_decl))
            .expect("导入的 answer 原型已注入");
        let exported_def = merged.iter().position(|d| {
            matches!(
                d,
                CppDecl::ExportBlock(inner)
                    if matches!(inner.as_slice(), [CppDecl::FreeFn { name, .. }] if name == "get")
            )
        });
        let exported_def = exported_def.expect("get 仍以 ExportBlock 形态存在");
        assert!(
            injected < exported_def,
            "导入声明 (idx {}) 必须在带定义的 export (idx {}) 之前",
            injected,
            exported_def
        );
    }

    #[test]
    fn fn_signature_and_layout_round_trip() {
        let sig = make_fn_signature_ex(
            &MangleType::Int,
            "_ZN5Shape3areav",
            &[MangleType::Pointer(Box::new(MangleType::Class("Shape".to_string())))],
            true,
            true,
        );
        assert!(sig.ends_with(" @virtual @const"), "{}", sig);
        let (ret, params, is_v, is_c) = parse_fn_signature_ex(&sig).expect("parse sig");
        assert_eq!(ret, MangleType::Int);
        assert_eq!(
            params,
            vec![MangleType::Pointer(Box::new(MangleType::Class("Shape".to_string())))]
        );
        assert!(is_v && is_c);
        // 虚/const 差异必须影ODR 比较 (签名文本不同)
        let plain = make_fn_signature(
            &MangleType::Int,
            "_ZN5Shape3areav",
            &[MangleType::Pointer(Box::new(MangleType::Class("Shape".to_string())))],
            false,
        );
        assert_ne!(sig, plain);
        // 多词类型 / 指针 / 引用 / 数组 往返
        for ty in [
            MangleType::Int,
            MangleType::Pointer(Box::new(MangleType::Char)),
            MangleType::Reference(Box::new(MangleType::LongLong)),
            MangleType::Array(Box::new(MangleType::Double), 4),
            MangleType::Class("Widget".to_string()),
        ] {
            let text = ty_to_c(&ty);
            assert_eq!(c_to_ty(&text), Some(ty.clone()), "{}", text);
        }
        // 字段布局文本与 class_exports 的写法一致 (`T[N] name;` 数组名在后)
        assert_eq!(
            parse_layout("int id; struct Widget next; double[3] x;"),
            vec![
                ("id".to_string(), MangleType::Int),
                ("next".to_string(), MangleType::Class("Widget".to_string())),
                ("x".to_string(), MangleType::Array(Box::new(MangleType::Double), 3)),
            ]
        );
    }

    #[test]
    fn odr_conflicts_reports_cross_module_mismatch_only() {
        let mk = |name: &str, sig: &str| BmfModule {
            name: name.to_string(),
            exports: vec![BmfExport {
                kind: BmfKind::Function,
                name: "foo".to_string(),
                signature: sig.to_string(),
                layout: None,
            }],
            imports: vec![],
            partition: None,
            part_imports: vec![],
        };
        let moda = mk("moda", "int _Z3foov()");
        // M28.6: 比较键是 mangled 链接名 —— 重载 (`foo()` vs `foo(int)`) 不撞键;
        // 真实违例是「同 mangled 名 + 不同类型文本」(Itanium 不编返回类型)
        let modb_diff = mk("modb", "long _Z3foov()");
        let modb_same = mk("modb", "int _Z3foov()");
        // 同键不同签名 -> 违规, 文本含实体名与两侧模块名
        let msg = odr_conflicts(None, &[moda.clone(), modb_diff.clone()]).expect("violation");
        assert!(msg.contains("ODR violations"), "{}", msg);
        assert!(msg.contains("_Z3foov"), "{}", msg);
        assert!(msg.contains("moda") && msg.contains("modb"), "{}", msg);
        // 同名同签名 = 同一实体重复声明, 合法
        assert!(odr_conflicts(None, &[moda.clone(), modb_same.clone()]).is_none());
        // 本 TU 导出与导入模块冲突也要报 (模块环 / 错误重定义)
        assert!(odr_conflicts(Some(&moda), &[modb_diff]).is_some());
        assert!(odr_conflicts(Some(&moda), &[modb_same]).is_none());
        // 只依赖单个模块时不可能冲突
        assert!(odr_conflicts(None, &[mk("only", "int _Z3foov()")]).is_none());
    }

    #[test]
    fn load_imported_modules_resolves_module_and_partition_files() {
        let dir = std::env::temp_dir().join(format!("dcc-modules-unit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut part = BmfModule {
            name: "mymod".to_string(),
            partition: Some("part".to_string()),
            ..Default::default()
        };
        part.exports.push(BmfExport {
            kind: BmfKind::Function,
            name: "pfn".to_string(),
            signature: "int _Z3pfnv()".to_string(),
            layout: None,
        });
        std::fs::write(dir.join("mymod.part.bmf"), part.serialize()).unwrap();
        std::fs::write(dir.join("mymod.bmf"), "DCC_BMF_V1\nM mymod\nE F get int _Z3getv()\n")
            .unwrap();

        // `import :part;` -> `<模块名>.part.bmf`
        let decls = parse_decls("export module mymod;\nimport :part;\n");
        let loaded = load_imported_modules(&decls, std::slice::from_ref(&dir));
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].partition.as_deref(), Some("part"));
        assert_eq!(loaded[0].exports[0].name, "pfn");

        // `import mymod;` -> `mymod.bmf`
        let decls = parse_decls("import mymod;\nint main() { return get(); }\n");
        let loaded = load_imported_modules(&decls, std::slice::from_ref(&dir));
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "mymod");
        assert_eq!(loaded[0].exports[0].name, "get");

        // 搜索目录均无 BMF -> 空 (不阻断编译)
        assert!(load_imported_modules(&decls, &[]).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
