//! C++ 编译驱动 (M7 最小).
//!
//! 流程: 读入 C++ 源 -> C 预处理器 (展开 #include) -> lex/parse ->
//! lower 到 C 字符串 -> 写到临时 .c 文件 -> 调 dcc C 驱动编译.
//!
//! 限制: M7 阶段只支持 .cpp → .o 一次性编译, 不支持单独头文件.
//! M11 stdlib: 默认 include 路径加入 src/cpp/stdlib/headers,
//!   允许 `#include <string>` / `#include <vector>` 等直接被解析.

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use crate::cpp::lower::Lower;
use crate::cpp::parser::CppParser;
use crate::cpp::CppError;

/// M11 stdlib 默认 include 路径 (含本 crate 的 stdlib/headers)
pub fn default_stdlib_include() -> Option<PathBuf> {
    let p = PathBuf::from("src/cpp/stdlib/headers");
    if p.is_dir() {
        Some(p)
    } else {
        None
    }
}

/// 编译 C++ 源到 .o
pub fn compile_cpp(
    cpp_src: &Path,
    obj_out: &Path,
    dcc_bin: &Path,
    include_paths: &[String],
    opt_level: u8,
) -> Result<(), CppError> {
    let src = fs::read_to_string(cpp_src).map_err(|e| {
        CppError::Io(std::io::Error::new(
            e.kind(),
            format!("read {}: {}", cpp_src.display(), e),
        ))
    })?;
    let bytes = src.as_bytes();
    let file = cpp_src.to_str().unwrap_or("input.cpp");

    // M11 stdlib: 先用 dcc C 预处理器展开 #include / #define / #ifndef
    // 等预处理指令, 这样 dcc++ 的 parser 就可以处理已展开的源.
    // 始终把 stdlib headers 加到 include 路径, 允许 #include <string>.
    let mut all_includes: Vec<PathBuf> = include_paths.iter().map(PathBuf::from).collect();
    if let Some(stdlib_inc) = default_stdlib_include() {
        all_includes.push(stdlib_inc);
    }
    let pp_out = crate::preproc::preprocess(
        bytes,
        file,
        &all_includes,
        &[], /* system paths */
        &[], /* defines */
        crate::cli::StdMode::Gnu11,
        &[],                                    /* force includes */
        false,                                  /* freestanding */
        &crate::preproc::PchOptions::default(), /* PCH */
        false,                                  /* kernel_code_model */
        false,                                  /* short_wchar */
        &crate::cli::TargetTriple::host(),      /* target */
    );
    if !pp_out.diags.is_empty() {
        let mut out = Vec::new();
        for d in &pp_out.diags {
            out.push(d.clone());
        }
        return Err(CppError::Diagnostics(out));
    }
    let preprocessed = pp_out.text;
    let preprocessed_bytes = preprocessed.clone().into_bytes();

    let mut parser = CppParser::new(&preprocessed_bytes, file);
    let decls = parser
        .parse_top_level()
        .map_err(|e| CppError::Diagnostics(vec![e.to_diag(file)]))?;
    let mut lower = Lower::new();
    lower.lower_decls(&decls);
    let c_src = lower.finish();

    // 写到临时 .c 文件, 调 dcc 编译后删除
    let tmp = std::env::temp_dir().join(format!(
        "dcc-cpp-{}-{}.c",
        std::process::id(),
        cpp_src
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("input")
    ));
    fs::write(&tmp, c_src.clone())?;

    // 调 dcc 编译
    let mut cmd = Command::new(dcc_bin);
    cmd.arg("-c");
    cmd.arg("-fpermissive");
    cmd.arg(format!("-O{}", opt_level));
    for inc in include_paths {
        cmd.arg("-I").arg(inc);
    }
    cmd.arg("-o").arg(obj_out);
    cmd.arg(&tmp);

    let out = cmd
        .output()
        .map_err(|e| CppError::Io(std::io::Error::new(e.kind(), format!("spawn dcc: {}", e))))?;
    let _ = fs::remove_file(&tmp);
    if !out.status.success() {
        let mut span = crate::diagnostics::Span::point(file, 0, 0);
        span.file = file.to_string();
        return Err(CppError::Diagnostics(vec![
            crate::diagnostics::Diagnostic::err(
                "C0100",
                format!("dcc failed: {}", String::from_utf8_lossy(&out.stderr)),
                span,
            ),
        ]));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpp::lower::Lower;
    use crate::cpp::parser::CppParser;

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
        assert!(
            s.contains("struct Counter"),
            "missing struct Counter in: {}",
            s
        );
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
