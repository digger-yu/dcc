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
        }
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
    assert!(
        parse_header("type_traits", &src),
        "type_traits should parse"
    );
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
        Ok(_) => {}
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
    assert!(
        parse_header("initializer_list", &src),
        "initializer_list should parse"
    );
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
            Ok(_) => {}
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

