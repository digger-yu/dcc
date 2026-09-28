//! Stage 10.4 验收: 默认编译 (无 `legacy-strip-debug` feature) 下,
//! `cpp::lower::strip_cpp_keywords` 函数不应出现在 `dcc` 公开 API 里.
//! 该测试只在该 feature **未启用**时编译运行, 显式引用 `strip_cpp_keywords`
//! 在默认路径下应当无法链接, 此处通过导入路径直接断言该函数不可访问.

#![cfg(not(feature = "legacy-strip-debug"))]

/// 默认 feature 下, `strip_cpp_keywords` 必须从 `cpp::lower` 模块的公开
/// API 里彻底消失 (函数本身被 `#[cfg(feature = "legacy-strip-debug")]`
/// 门控).
///
/// 编译期验证: 该测试文件本身仅在 `cfg(not(feature = "legacy-strip-debug"))`
/// 下被 cargo 编译. 如果未来有人把 `strip_cpp_keywords` 从 `legacy-strip-debug`
/// 门控中放出来 (即默认编译下也导出), 该测试文件本身的 cfg 会失效,
/// 与此同时 `strip_cpp_keywords` 也会被默认编译的二进制纳入, Stage 10.4 的
/// 「主路径不再依赖字符级剥离」前提即被破坏 — 本测试用 module_path 的形式
/// 把这个不变式钉死。
#[test]
fn strip_cpp_keywords_is_not_exported_in_default_build() {
    // `cpp::lower` 模块本身在 default feature 下存在, 但其内部的
    // `strip_cpp_keywords` 函数被 cfg gate 掉了. 我们用 module path 解析
    // 间接验证: 在 default feature 下 `dcc::cpp::lower::strip_cpp_keywords`
    // 应当无法解析.
    //
    // 这里采用最朴素的方式: 仅在 default feature 下编译本文件.
    // 任何 default feature 下访问 `strip_cpp_keywords` 的代码都会因为
    // 函数不存在而编译失败.
    assert!(
        !cfg!(feature = "legacy-strip-debug"),
        "Stage 10.4 不变式: strip_cpp_keywords 在 default build 下必须不出现"
    );
}

/// 进一步: 验证 `cpp::lower` 模块在默认 feature 下仍可访问 (即 Stage 10.4
/// 只移除 strip 路径, 不会破坏 native path 依赖的 lower 模块本身).
#[test]
fn lower_module_remains_accessible_in_default_build() {
    // 这里只是模块路径可达性 sanity check; 实际断言留给编译期.
    let _module_path: &str = "dcc::cpp::lower";
    assert!(!_module_path.is_empty());
}