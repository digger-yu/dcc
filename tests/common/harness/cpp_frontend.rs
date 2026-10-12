//! C++ 前端侧助手：native（sema/HIR）与 legacy C 文本降级的**对照键**定义。
//!
//! 由 `tests/common/harness.rs` 挂载，经 `pub(crate) use` 注入 integration crate root，
//! 供 `tests/integration/cpp_runtime.rs`（Stage 2 三向对照）与
//! `tests/integration/cpp_lang.rs`（变参模板 AST 构造）复用。
//!
//! 对照轴取自**同一份 AST**：源码里写出定义（`!body.is_decl`）的每个实体都必须
//! 1. 在 native 侧被登记（`sema` 符号表 + `hir.functions` 定义集），
//! 2. 在 legacy 侧被真的发成定义行，
//! 3. legacy 以普通名字发出的定义不得超出 native 认识的实体。
//!
//! 为什么不直接比链接名：两条路径的命名口径现在就不一样 —— legacy 发 Itanium mangled
//! （`_ZN7Counter4bumpEv`）并把命名空间展平成 `geo_Point`，native 普通 TU 发后端约定名
//! （`Counter_bump`）并保留 `geo::Point` 作用域链；legacy 还存在同一函数发两种 mangled
//! 形态的历史毛病。因此比对键取 **(角色, 实体基名)**。
//!
//! native 可以合法地比源码**多出**编译器合成的实体（隐式 ctor/dtor、vtable、thunk），
//! 那部分由 `tests/fixtures/cpp_abi_*` 与 g++ 逐行交叉验证覆盖，不在本对照的断言范围。

use std::collections::BTreeSet;

use dcc::cpp::hir::HirTranslationUnit;
use dcc::cpp::legacy;
use dcc::cpp::parser::{ClassDecl, CppBody, CppDecl, CppParser};
use dcc::cpp::sema::{CxxSymbol, CxxSymbolKind, Sema};

/// 构造一个空成员的 `ClassDecl` (G22 起 `member_access` 是必选字段,
/// 合成的模板体没有访问说明符, 表为空 -> sema 按语言默认回退).
pub(crate) fn empty_class(name: &str, is_struct: bool) -> ClassDecl {
    ClassDecl {
        name: name.to_string(),
        is_struct,
        is_union: false,
        bases: vec![],
        members: vec![],
        member_access: vec![],
    }
}

/// 实体角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Role {
    /// 自由函数或成员函数。二者在对照里合并: legacy 把 `ns::f` 的限定前缀一律当成类名
    /// (发成 `_ZN3geo5totalE (struct geo* this_, ...)`), native 按 [basic.def.outside]
    /// 正确识别为命名空间作用域自由函数 —— 角色差异是 legacy 的已知建模差异, 不是本轮
    /// 要钉住的不变式, **实体存在性**才是。
    Callable,
    Ctor,
    Dtor,
}

/// 对照键 = (角色, 实体基名)。
pub(crate) type Entity = (Role, String);

/// 运行时/后端自身的符号, 不参与对照 (只有一条路径会有它们)。
pub(crate) fn is_backend_internal(name: &str) -> bool {
    name.starts_with("__cxa_")
        || name.starts_with("_cxa_")
        || name.starts_with("__dcc_")
        || name.starts_with("dccpp_")
        || name.starts_with("_Unwind_")
        || name.starts_with("__gxx_")
        || name.starts_with("__gcc_")
        || name.starts_with("__dynamic_cast")
        || name.starts_with("__register_frame")
        || name.starts_with("__deregister_frame")
        || name.ends_with("__attribute__")
}

/// 函数/方法实体键: 名字本身。**不做剥前缀处理** —— 两侧的可调用实体名都是不带限定的
/// 声明名 (`via_base` / `total`), 剥 `_` 前缀会把 `via_base` 误归成 `base`。
pub(crate) fn callable_key(name: &str) -> String {
    name.to_string()
}

/// 类实体键 (ctor/dtor 用): 剥掉命名空间限定。
///
/// native 保留 `geo::Point`, legacy 把命名空间展平成 `geo_Point`, 所以这里 `::` 与 `_`
/// 两层前缀都剥, 只保留类名本体。代价: 仅命名空间段不同的同名类会折叠成一个键 ——
/// 样本里不存在这种重名, 而且 `every_native_class_has_a_legacy_struct_definition` 会
/// 另行逐个类检查 legacy 是否发了对应的 `struct`。
pub(crate) fn class_key(name: &str) -> String {
    let after_ns = name.rsplit("::").next().unwrap_or(name);
    after_ns.rsplit('_').next().unwrap_or(after_ns).to_string()
}

/// 从 AST 收集「源码里写了函数体的实体」—— 对照的基准集合。
pub(crate) fn expected_entities(decls: &[CppDecl]) -> BTreeSet<Entity> {
    let mut out = BTreeSet::new();
    collect_expected(decls, &mut out);
    out
}

pub(crate) fn push_if_defined(out: &mut BTreeSet<Entity>, role: Role, name: &str, body: &CppBody) {
    if !body.is_decl {
        let key = match role {
            Role::Callable => callable_key(name),
            Role::Ctor | Role::Dtor => class_key(name),
        };
        out.insert((role, key));
    }
}

pub(crate) fn collect_expected(decls: &[CppDecl], out: &mut BTreeSet<Entity>) {
    for d in decls {
        match d {
            CppDecl::FreeFn { name, body, .. } => push_if_defined(out, Role::Callable, name, body),
            CppDecl::MemberFn { name, body, .. } => {
                // const 限定的类外定义 (`int Point::sum() const`) 在 legacy 里可能同时
                // 存在带 `_K` 后缀与不带的两份编码, 但**基名**只有一个实体, 无需区分。
                push_if_defined(out, Role::Callable, name, body);
            },
            CppDecl::Ctor { class, body, .. } => push_if_defined(out, Role::Ctor, class, body),
            CppDecl::Dtor { class, body, .. } => push_if_defined(out, Role::Dtor, class, body),
            // 运算符重载不参与本对照: 其实体名是编码后的 `operator<sym>`
            // (native: `operatorpl`; legacy: `std_operatorpl` 一类展平名),
            // 名字形态不同不是「实体存在性」问题; 运算符的真实行为由
            // `tests/fixtures/cpp_abi_*` 的 g++ 逐行交叉验证钉住。
            CppDecl::OperatorFn { .. } => {},
            CppDecl::Class(class_decl) => collect_expected(&class_decl.members, out),
            CppDecl::Namespace { body, .. } => collect_expected(body, out),
            _ => {},
        }
    }
}

/// 同一份源码分别过两条路径。
pub(crate) fn lower_both(src: &str) -> (Vec<CppDecl>, Vec<CxxSymbol>, HirTranslationUnit, String) {
    let mut parser = CppParser::new(src.as_bytes(), "correspondence.cpp");
    let decls = parser
        .parse_top_level()
        .unwrap_or_else(|e| panic!("解析失败: {e:?}"));
    let constexpr_fns = parser.constexpr_fns().clone();
    let opaque: Vec<String> = decls
        .iter()
        .filter_map(|d| match d {
            CppDecl::Opaque(t) => Some(t.clone()),
            _ => None,
        })
        .collect();
    assert!(
        opaque.is_empty(),
        "样本里有被降级成 Opaque 的声明, 对照输入本身不完整: {opaque:?}"
    );

    let mut sema = Sema::new();
    sema.build_hir_from_decls(&decls)
        .unwrap_or_else(|e| panic!("native sema 拒绝该 TU: {e}"));
    let symbols = sema.symbols().to_vec();
    let hir = sema.into_hir();

    let legacy_src = legacy::lower_to_c(&decls, constexpr_fns)
        .unwrap_or_else(|e| panic!("legacy 降级失败: {e}"));
    (decls, symbols, hir, legacy_src)
}

/// native 侧「定义了」的实体集合。
///
/// `sema` 只为**有函数体**的声明建 `HirFunction` (`body.is_decl` 路径直接返回), 所以
/// `hir.functions` 就是定义集; 用户写的空体 `~Counter() {}` 也算定义 (stmts 为空但确实
/// 占一个符号), 不能按语句个数再筛。
pub(crate) fn native_entities(symbols: &[CxxSymbol], hir: &HirTranslationUnit) -> BTreeSet<Entity> {
    let defined: BTreeSet<usize> = hir.functions.iter().map(|f| f.symbol.0).collect();
    symbols
        .iter()
        .filter(|s| defined.contains(&s.id.0))
        .filter_map(|s| match s.kind {
            CxxSymbolKind::Function | CxxSymbolKind::Method => {
                if is_backend_internal(&s.name) {
                    return None;
                }
                Some((Role::Callable, callable_key(&s.name)))
            },
            CxxSymbolKind::Constructor => Some((Role::Ctor, class_key(&owner_name(symbols, s)?))),
            CxxSymbolKind::Destructor => Some((Role::Dtor, class_key(&owner_name(symbols, s)?))),
            _ => None,
        })
        .collect()
}

pub(crate) fn owner_name(symbols: &[CxxSymbol], sym: &CxxSymbol) -> Option<String> {
    let owner = sym.owner?;
    symbols
        .iter()
        .find(|x| x.id == owner)
        .map(|s| s.name.clone())
}

/// legacy 产物里的「定义行」: 形参表收尾即接 `{` 的那些行。
///
/// legacy 的函数定义几乎全部发成 `static inline <ret> <name> (args) {`, 所以不能把
/// `static inline` 排除掉。
pub(crate) fn legacy_definition_lines(src: &str) -> Vec<String> {
    src.lines()
        .map(str::trim)
        .filter(|l| l.contains(") {"))
        .filter(|l| !l.starts_with('#') && !l.starts_with("//"))
        .map(|l| l.to_string())
        .collect()
}

/// 一行里是否提到了实体名 `name`。
///
/// mangled 名把长度编码夹在名字前面 (`_ZN7Counter4bumpEv`), 所以整词匹配之后还要允许
/// 「以 `<name>Ev` / `<name>E` 结尾」这种嵌入形态 (但不会让 `sum` 命中 `summary`)。
pub(crate) fn line_mentions(line: &str, name: &str) -> bool {
    line.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|tok| {
            tok == name || tok.ends_with(&format!("{name}Ev")) || tok.ends_with(&format!("{name}E"))
        })
}

/// 实体是否被 legacy 发成了定义。
pub(crate) fn legacy_defines(lines: &[String], e: &Entity) -> bool {
    match e {
        (Role::Callable, name) => lines.iter().any(|l| line_mentions(l, name)),
        // ctor/dtor 的实体名就是类名: mangled 形态是 `<类名>C1Ev` (complete) / `C2Ev`
        // (base) 与 `D0/D1/D2Ev`, legacy 也可能发展平形态 `<类名>_ctor_*`。
        // 这里用**子串**而不是整词匹配: 命名空间展平后类名在 legacy 里是 `geo_Point`,
        // 而基准集里的键是 `Point` —— `_ZN9geo_PointC1Ev` 含 `PointC1Ev`。
        (Role::Ctor, class) => lines.iter().any(|l| {
            l.contains(&format!("{class}C1Ev"))
                || l.contains(&format!("{class}C2Ev"))
                || l.contains(&format!("{class}_ctor"))
        }),
        (Role::Dtor, class) => lines.iter().any(|l| {
            l.contains(&format!("{class}D0Ev"))
                || l.contains(&format!("{class}D1Ev"))
                || l.contains(&format!("{class}D2Ev"))
                || l.contains(&format!("{class}_dtor"))
        }),
    }
}

/// legacy 定义行上的**未 mangled** 函数名 (展平形态或普通 C 名)。
pub(crate) fn legacy_plain_definition_names(lines: &[String]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for l in lines {
        let Some(open) = l.find('(') else { continue };
        let head = l[..open].trim_end();
        let name: String = head
            .chars()
            .rev()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$' || *c == ':')
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if name.is_empty() || name.contains("_Z") || is_backend_internal(&name) {
            continue;
        }
        if matches!(name.as_str(), "if" | "while" | "for" | "switch" | "return" | "sizeof") {
            continue;
        }
        out.insert(name);
    }
    out
}

/// 对照样本: 覆盖 Stage 2 声明的实体种类 (free function / 成员函数 / ctor / dtor /
/// const 方法 / 命名空间 / 类外定义 / 继承 + 虚函数)。
pub(crate) const CORPUS: &[&str] = &[
    // 1. 普通 class + ctor + 成员函数 + dtor
    r#"
class Counter {
public:
  Counter() : n(0) {}
  int get() const { return n; }
  void bump() { n = n + 1; }
  ~Counter() {}
private:
  int n;
};
int useit(Counter& c) { c.bump(); return c.get(); }
"#,
    // 2. 命名空间 + 类外定义 (成员与命名空间作用域自由函数各一)
    r#"
namespace geo {
struct Point {
  int x;
  int y;
  Point() : x(0), y(0) {}
  int sum() const;
};
int total(const Point& p);
}
int geo::Point::sum() const { return x + y; }
int geo::total(const geo::Point& p) { return p.sum(); }
"#,
    // 3. 继承 + 虚函数 (legacy 用 C 文本模拟 vtable, native 用 ABI 布局)
    r#"
struct Base {
  int b;
  Base() : b(1) {}
  virtual int value() { return b; }
  virtual ~Base() {}
};
struct Derived : Base {
  int d;
  Derived() : d(2) {}
  int value() override { return b + d; }
};
int via_base(Base* p) { return p->value(); }
"#,
];
