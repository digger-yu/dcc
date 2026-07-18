//! dcc++ 标准库模块单元测试
use super::*;

#[test]
fn stdlib_constants_in_range() {
    assert!(STDLIB_ENABLED);
    assert!(!STD_HEADERS.is_empty());
    assert!(STD_HEADERS.contains(&"string"));
    assert!(STD_HEADERS.contains(&"vector"));
    assert!(STD_HEADERS.contains(&"iostream"));
    assert!(STD_HEADERS.contains(&"memory"));
}

#[test]
fn stdlib_runtime_c_path_resolves() {
    assert!(RUNTIME_C_PATH.ends_with("dccpp_runtime.c"));
}

#[test]
fn stdlib_header_dir_is_correct() {
    assert!(HEADER_DIR.starts_with("src/"));
    assert!(HEADER_DIR.ends_with("headers"));
}

#[test]
fn emit_stdlib_includes_has_all_headers() {
    let s = emit_stdlib_includes();
    for h in STD_HEADERS {
        assert!(s.contains(h), "missing include for {}: {}", h, s);
    }
}

#[test]
fn stdlib_headers_count_matches() {
    assert_eq!(STD_HEADERS.len(), 16);
}

#[test]
fn stdlib_headers_sorted_unique() {
    let mut v: Vec<&str> = STD_HEADERS.to_vec();
    v.sort();
    let mut orig_s = STD_HEADERS.to_vec();
    orig_s.sort();
    assert_eq!(v, orig_s, "STD_HEADERS not sorted/unique");
}

#[test]
fn emit_stdlib_includes_uses_angle_brackets() {
    let s = emit_stdlib_includes();
    assert!(s.contains("#include <string>"));
    assert!(s.contains("#include <vector>"));
    assert!(s.contains("#include <iostream>"));
}

#[test]
fn stdlib_headers_have_namespace_std() {
    let headers = [
        ("string", "namespace std"),
        ("vector", "namespace std"),
        ("iostream", "namespace std"),
        ("memory", "namespace std"),
        ("exception", "namespace std"),
        ("stdexcept", "namespace std"),
        ("typeinfo", "namespace std"),
        ("initializer_list", "namespace std"),
        ("utility", "namespace std"),
        ("algorithm", "namespace std"),
        ("map", "namespace std"),
        ("functional", "namespace std"),
    ];
    for (h, pat) in headers.iter() {
        let path = format!("src/cpp/stdlib/headers/{}", h);
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {}", path, e));
        assert!(src.contains(pat),
            "{}: missing `{}` (not in std namespace)", h, pat);
    }
}

#[test]
fn stdlib_runtime_c_braces_balanced() {
    let src = std::fs::read_to_string(RUNTIME_C_PATH)
        .expect("read runtime");
    let open_b = src.matches('{').count();
    let close_b = src.matches('}').count();
    assert_eq!(open_b, close_b,
        "unbalanced braces in runtime: {} open, {} close", open_b, close_b);
    let open_p = src.matches('(').count();
    let close_p = src.matches(')').count();
    assert_eq!(open_p, close_p,
        "unbalanced parens in runtime: {} open, {} close", open_p, close_p);
}
