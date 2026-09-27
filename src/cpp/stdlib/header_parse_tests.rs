//! dcc++ C++ 标准库集成测试
//!
//! 测试 C++ 头文件能否被 dcc++ 解析器接受.
//! 完整端到端 (parse → lower → dcc compile → run) 测试在
//! 远程 Linux 跑 (tests/integration.rs).

use crate::cpp::parser::CppParser;

fn parse_header(name: &str, src: &str) -> bool {
    let mut p = CppParser::new(src.as_bytes(), name);
    match p.parse_top_level() {
        Ok(_) => true,
        Err(e) => {
            eprintln!("[{}] parse error: {:?}", name, e);
            false
        },
    }
}

#[test]
fn header_cstddef_parses() {
    let src = std::fs::read_to_string("src/cpp/stdlib/headers/cstddef").expect("read cstddef");
    assert!(parse_header("cstddef", &src), "cstddef should parse");
}

#[test]
fn header_cstdlib_parses() {
    let src = std::fs::read_to_string("src/cpp/stdlib/headers/cstdlib").expect("read cstdlib");
    assert!(parse_header("cstdlib", &src), "cstdlib should parse");
}

#[test]
fn header_cstring_parses() {
    let src = std::fs::read_to_string("src/cpp/stdlib/headers/cstring").expect("read cstring");
    assert!(parse_header("cstring", &src), "cstring should parse");
}

#[test]
fn header_cstdio_parses() {
    let src = std::fs::read_to_string("src/cpp/stdlib/headers/cstdio").expect("read cstdio");
    assert!(parse_header("cstdio", &src), "cstdio should parse");
}

#[test]
fn header_type_traits_parses() {
    // M30.1: type_traits 头 (conditional/enable_if 非类型模板实参,
    // is_same 偏特化, enum value 成员) 应完整解析
    let src =
        std::fs::read_to_string("src/cpp/stdlib/headers/type_traits").expect("read type_traits");
    assert!(parse_header("type_traits", &src), "type_traits should parse");
}

#[test]
fn parse_relational_lt_not_template() {
    // 回归: `av < bv` 曾被误判为模板实参而无限循环
    let src = b"namespace std { class string { int operator<(const string& o) const {
        const char* a = 0; const char* b = 0;
        while (*a && *b && *a == *b) { a++; b++; }
        int av = *a; int bv = *b; return av < bv;
    } }; }";
    let mut p = CppParser::new(src, "string");
    p.parse_top_level().expect("relational lt should parse");
}

#[test]
fn header_string_parses() {
    let src = std::fs::read_to_string("src/cpp/stdlib/headers/string").expect("read string");
    let mut p = CppParser::new(src.as_bytes(), "string");
    match p.parse_top_level() {
        Ok(_) => {},
        Err(e) => panic!("string header parse error: {:?}", e),
    }
}

#[test]
fn diag_ns_empty_class() {
    parse_header("ns_empty_class", "namespace std { class string {}; }");
}

#[test]
fn header_vector_parses() {
    let src = std::fs::read_to_string("src/cpp/stdlib/headers/vector").expect("read vector");
    assert!(parse_header("vector", &src), "vector should parse");
}

#[test]
fn header_iostream_parses() {
    let src = std::fs::read_to_string("src/cpp/stdlib/headers/iostream").expect("read iostream");
    assert!(parse_header("iostream", &src), "iostream should parse");
}

#[test]
fn header_memory_parses() {
    let src = std::fs::read_to_string("src/cpp/stdlib/headers/memory").expect("read memory");
    assert!(parse_header("memory", &src), "memory should parse");
}

#[test]
fn header_exception_parses() {
    let src = std::fs::read_to_string("src/cpp/stdlib/headers/exception").expect("read exception");
    assert!(parse_header("exception", &src), "exception should parse");
}

#[test]
fn header_stdexcept_parses() {
    let src = std::fs::read_to_string("src/cpp/stdlib/headers/stdexcept").expect("read stdexcept");
    assert!(parse_header("stdexcept", &src), "stdexcept should parse");
}

#[test]
fn header_typeinfo_parses() {
    let src = std::fs::read_to_string("src/cpp/stdlib/headers/typeinfo").expect("read typeinfo");
    assert!(parse_header("typeinfo", &src), "typeinfo should parse");
}

#[test]
fn header_initializer_list_parses() {
    let src = std::fs::read_to_string("src/cpp/stdlib/headers/initializer_list")
        .expect("read initializer_list");
    assert!(parse_header("initializer_list", &src), "initializer_list should parse");
}

#[test]
fn header_utility_parses() {
    let src = std::fs::read_to_string("src/cpp/stdlib/headers/utility").expect("read utility");
    assert!(parse_header("utility", &src), "utility should parse");
}

#[test]
fn header_algorithm_parses() {
    let src = std::fs::read_to_string("src/cpp/stdlib/headers/algorithm").expect("read algorithm");
    assert!(parse_header("algorithm", &src), "algorithm should parse");
}

#[test]
fn header_map_parses() {
    let src = std::fs::read_to_string("src/cpp/stdlib/headers/map").expect("read map");
    assert!(parse_header("map", &src), "map should parse");
}

#[test]
fn header_functional_parses() {
    let src =
        std::fs::read_to_string("src/cpp/stdlib/headers/functional").expect("read functional");
    assert!(parse_header("functional", &src), "functional should parse");
}

#[test]
fn all_stdlib_headers_parse() {
    let headers = crate::cpp::stdlib::STD_HEADERS;
    for h in headers {
        let path = format!("src/cpp/stdlib/headers/{}", h);
        let src = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => panic!("cannot read {}: {}", path, e),
        };
        let mut p = CppParser::new(src.as_bytes(), h);
        match p.parse_top_level() {
            Ok(_) => {},
            Err(e) => panic!("header {} failed to parse: {:?}", h, e),
        }
    }
}

#[test]
fn stdlib_string_class_has_cstr_method() {
    use crate::cpp::parser::*;
    let src = b"
        namespace std { class string {
        public:
            string();
            string(const char*);
            const char* c_str() const;
            size_t size() const;
        }; }
    ";
    let mut p = CppParser::new(src, "test");
    let _ = p.parse_top_level().expect("parse");
}

#[test]
fn stdlib_vector_class_has_push_back() {
    use crate::cpp::parser::*;
    let src = b"
        namespace std { class vector {
        public:
            vector();
            void push_back_bytes(const void*);
            void* at(size_t);
            size_t size() const;
        }; }
    ";
    let mut p = CppParser::new(src, "test");
    let _ = p.parse_top_level().expect("parse");
}

#[test]
fn stdlib_iostream_cout_usage_parses() {
    // 模拟 std::cout() << \"hello\" 调用语法
    use crate::cpp::parser::*;
    let src = b"
        namespace std {
            class ostream {
            public:
                ostream& operator<<(const char*);
                ostream& operator<<(int);
            };
            ostream& cout();
        }
        int main() {
            std::cout() << \"hello\";
            return 0;
        }
    ";
    let mut p = CppParser::new(src, "test.cpp");
    let decls = p.parse_top_level().expect("parse");
    let has_main = decls.iter().any(|d| {
        matches!(d,
            crate::cpp::parser::CppDecl::FreeFn { name, .. } if name == "main"
        )
    });
    assert!(has_main, "expected main function");
}

// ============================================================================
// 头部「非空过」守卫: Opaque 降级检测
//
// 类成员循环遇到无法识别的声明时, 会把从该点起的**整段剩余文本**降级成
// `CppDecl::Opaque` 占位, 而 `parse_top_level()` 照样返回 Ok —— 只看 Ok 的
// 头文件测试会静默空过 (字段/方法就这样丢了)。真实回归:
//   - `<iostream>` 的 manipulator 重载 `ostream& operator<<(ostream& (*)(ostream&))`
//     被吞 -> 夹具 iostream_basic.cpp 报 "this used outside member function";
//   - `int operator++()` / `operator!` / `operator~` 之后的数据成员被吞
//     -> 夹具 iterator_traits.cpp 的 `CountIt::p_` 报 "undeclared identifier".
// ============================================================================

use crate::cpp::parser::CppDecl;

fn parse_decls(name: &str, src: &str) -> Vec<CppDecl> {
    let mut p = CppParser::new(src.as_bytes(), name);
    match p.parse_top_level() {
        Ok(d) => d,
        Err(e) => panic!("{name} parse error: {e:?}"),
    }
}

/// 递归收集所有被降级成 Opaque 的片段 (类成员 / 命名空间体 / 模板体).
fn collect_opaque(decls: &[CppDecl], out: &mut Vec<String>) {
    for d in decls {
        match d {
            CppDecl::Opaque(s) => out.push(s.clone()),
            CppDecl::Class(c) => collect_opaque(&c.members, out),
            CppDecl::Namespace { body, .. } => collect_opaque(body, out),
            CppDecl::Template { decl, .. } | CppDecl::PartialSpec { decl, .. } => {
                collect_opaque(std::slice::from_ref(&**decl), out)
            },
            CppDecl::AnonAggr { members, .. } => collect_opaque(members, out),
            _ => {},
        }
    }
}

/// 把 AST 里所有类成员按 (类名, 成员名/运算符名) 扫出来.
fn class_member_names(decls: &[CppDecl], out: &mut Vec<(String, String)>) {
    for d in decls {
        match d {
            CppDecl::Class(c) => {
                for m in &c.members {
                    match m {
                        CppDecl::MemberFn { name, .. } => {
                            out.push((c.name.clone(), name.clone()))
                        },
                        CppDecl::OperatorFn { op, .. } => {
                            out.push((c.name.clone(), format!("operator{op:?}")))
                        },
                        CppDecl::MemberVar { name, .. } => {
                            out.push((c.name.clone(), name.clone()))
                        },
                        _ => {},
                    }
                }
            },
            CppDecl::Namespace { body, .. } => class_member_names(body, out),
            CppDecl::Template { decl, .. } | CppDecl::PartialSpec { decl, .. } => {
                class_member_names(std::slice::from_ref(&**decl), out)
            },
            _ => {},
        }
    }
}

#[test]
fn header_iostream_declares_every_member_without_opaque() {
    let src = std::fs::read_to_string("src/cpp/stdlib/headers/iostream").expect("read iostream");
    let decls = parse_decls("iostream", &src);
    let mut opaque = Vec::new();
    collect_opaque(&decls, &mut opaque);
    assert!(opaque.is_empty(), "<iostream> 中被降级的片段: {opaque:?}");

    let mut names = Vec::new();
    class_member_names(&decls, &mut names);
    // clog/ws 用例依赖的流成员与 manipulator 重载必须真的存在
    for want in ["flush", "put", "write", "get", "peek", "getline", "ignore", "eof"] {
        assert!(
            names.iter().any(|(_, n)| n == want),
            "<iostream> 缺流成员 {want}: {names:?}"
        );
    }
    // `<< std::endl` / `>> std::ws` 的 manipulator 重载 (函数指针形参)
    let shl = names
        .iter()
        .filter(|(c, n)| c == "ostream" && n.contains("Shl"))
        .count();
    let shr = names
        .iter()
        .filter(|(c, n)| c == "istream" && n.contains("Shr"))
        .count();
    assert!(shl >= 13, "ostream operator<< 重载数 {shl} 应含全部内建类型 + manipulator");
    assert!(shr >= 11, "istream operator>> 重载数 {shr} 应含数值/char*/manipulator");
}

#[test]
fn deleted_copy_members_are_parsed_not_swallowed() {
    // `<ranges>` 的 view 类都写 `X(const X&) = delete;` + `X& operator=(const X&) = delete;`
    // 来禁止拷贝。这两形状必须解析成真实成员 (不降成 CppDecl::Opaque), 否则
    // 头里的类在 sema 眼里就没这些函数, 用户写 `filter_view f2 = f;` 时既不会
    // 报错也拿不到删除信息 (实测整个头被静默吃掉一半)。
    let src = "class V {
public:
    V() : n_(0) {}
    V(const V&) = delete;
    V& operator=(const V&) = delete;
    int size() const { return n_; }
private:
    int n_;
};";
    let decls = parse_decls("deleted_copy", src);
    let mut opaque = Vec::new();
    collect_opaque(&decls, &mut opaque);
    assert!(opaque.is_empty(), "= delete 成员被降级: {opaque:?}");
    let mut names = Vec::new();
    class_member_names(&decls, &mut names);
    assert!(
        names.iter().any(|(c, n)| c == "V" && n == "size"),
        "= delete 之后的成员丢了: {names:?}"
    );
    assert!(
        names.iter().any(|(_, n)| n.contains("Assign")),
        "operator= 的 = delete 形式没被识别: {names:?}"
    );
}

#[test]
fn header_ranges_and_iterator_have_no_opaque_swallow() {
    for h in ["ranges", "iterator"] {
        let path = format!("src/cpp/stdlib/headers/{h}");
        let src = std::fs::read_to_string(&path).expect("read header");
        let decls = parse_decls(h, &src);
        let mut opaque = Vec::new();
        collect_opaque(&decls, &mut opaque);
        assert!(opaque.is_empty(), "<{h}> 中被降级的片段: {opaque:?}");
    }
}

/// 一元 operator (自增/自减/! / ~) 之后的数据成员不得丢失 —— 这既是
/// `<iterator>` 用户自定义迭代器 (CountIt) 的形状, 也是成员函数体能引用
/// 后置字段的前提。占 operator 名映射表 (mangle::from_tok_kind) 的回归。
#[test]
fn unary_operator_member_keeps_following_field() {
    let src = "class It {
    public:
        explicit It(int* p) : p_(p) {}
        int operator*() const { return *p_; }
        It& operator++() { ++p_; return *this; }
        int operator~() const { return 0; }
        int logical() const { return !p_; }
    private:
        int* p_;
    };";
    let decls = parse_decls("unary_op_field", src);
    let mut opaque = Vec::new();
    collect_opaque(&decls, &mut opaque);
    assert!(opaque.is_empty(), "类成员被降级: {opaque:?}");
    let mut names = Vec::new();
    class_member_names(&decls, &mut names);
    assert!(
        names.iter().any(|(c, n)| c == "It" && n == "p_"),
        "后置字段 p_ 丢了: {names:?}"
    );
    // operator++ (Inc) / operator* (Mul) / operator~ (Not) 都必须成类成员
    for op in ["Inc", "Mul", "Not"] {
        assert!(
            names.iter().any(|(_, n)| n.contains(op)),
            "operator {op} 没被识别: {names:?}"
        );
    }
}
