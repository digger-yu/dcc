//! C++ 编译驱动 (M7 最小).
//!
//! 流程: 读入 C++ 源 -> lex/parse -> lower 到 C 字符串 -> 写到
//! 临时 .c 文件 -> 调 dcc C 驱动编译.
//!
//! 限制: M7 阶段只支持 .cpp → .o 一次性编译, 不支持单独头文件.

use std::fs;
use std::path::Path;
use std::process::Command;

use crate::cpp::CppError;
use crate::cpp::lower::Lower;
use crate::cpp::parser::CppParser;

/// 编译 C++ 源到 .o
pub fn compile_cpp(
    cpp_src: &Path,
    obj_out: &Path,
    dcc_bin: &Path,
    include_paths: &[String],
    opt_level: u8,
) -> Result<(), CppError> {
    let src = fs::read_to_string(cpp_src)
        .map_err(|e| CppError::Io(std::io::Error::new(e.kind(), format!("read {}: {}", cpp_src.display(), e))))?;
    let bytes = src.as_bytes();
    let file = cpp_src.to_str().unwrap_or("input.cpp");
    let mut parser = CppParser::new(bytes, file);
    let decls = parser.parse_top_level()
        .map_err(|e| CppError::Diagnostics(vec![e.to_diag(file)]))?;
    let mut lower = Lower::new();
    lower.lower_decls(&decls);
    let c_src = lower.finish();

    // 写到临时 .c 文件
    let tmp = std::env::temp_dir().join(format!(
        "dcc-cpp-{}-{}.c",
        std::process::id(),
        cpp_src.file_name().and_then(|n| n.to_str()).unwrap_or("input")
    ));
    fs::write(&tmp, c_src)?;

    // 调 dcc 编译
    let mut cmd = Command::new(dcc_bin);
    cmd.arg("-c");
    cmd.arg(format!("-O{}", opt_level));
    for inc in include_paths { cmd.arg("-I").arg(inc); }
    cmd.arg("-o").arg(obj_out);
    cmd.arg(&tmp);

    let out = cmd.output()
        .map_err(|e| CppError::Io(std::io::Error::new(e.kind(), format!("spawn dcc: {}", e))))?;
    let _ = fs::remove_file(&tmp);
    if !out.status.success() {
        let mut span = crate::diagnostics::Span::point(file, 0, 0);
        span.file = file.to_string();
        return Err(CppError::Diagnostics(vec![crate::diagnostics::Diagnostic::err(
            "C0100",
            format!("dcc failed: {}", String::from_utf8_lossy(&out.stderr)),
            span,
        )]));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpp::parser::CppParser;
    use crate::cpp::lower::Lower;

    #[test]
    fn lower_simple_class_produces_c_output() {
        // 单元测试: 仅验证 lower 阶段产生预期的 C 字符串
        // 完整 dcc 集成编译测试在 tests/integration.rs 单独跑
        let src = b"class Counter { public: Counter() { } };";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(s.contains("struct Counter"), "missing struct Counter in: {}", s);
        assert!(s.contains("_ZN7CounterC1Ev"), "missing ctor in: {}", s);
    }

    #[test]
    fn lower_cpp_to_c_emits_valid_c_syntax() {
        // 测试 dtor + ctor 都能降级
        let src = b"class Foo { public: Foo() { } ~Foo() { } };";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(s.contains("struct Foo"), "{}", s);
        assert!(s.contains("_ZN3FooC1Ev"), "{}", s);
        assert!(s.contains("_ZN3FooD1Ev"), "{}", s);
    }
}
