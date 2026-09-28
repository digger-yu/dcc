//! Legacy C++ lowering 子模块
//!
//! Stage 1 拆分: 把 lower.rs 按职责拆分为多个子模块，行为保持不变。
//!
//! 子模块:
//! - runtime:       typedef / runtime prologue / extern 声明
//! - layout:        struct 布局与成员表模拟 (继承展开、前缀补齐、前置声明)
//! - expr:          C 表达式与语句形态降级 (成员访问、ctor/dtor 调用点、new/delete)
//! - name_flatten:  限定名 `A::b` 到 C 标识符的文本展平
//! - template_text: 模板定义剥离与 `X<Args>` 用法的字符串级实例化
//! - decl_cleanup:  顶层声明区清理 (属性、存储类、重复定义、泄漏语句)
//! - text_scan:     字节级 C 文本扫描原语 (字符串/注释/标识符/关键字)
//!
//! ## Stage 0: legacy 边界入口
//!
//! 本模块提供稳定的 legacy 入口 `lower_to_c`, 作为 `--cxx-native=off` /
//! `--cxx-native=auto` fallback 路径的真实入口. 该函数封装
//! `cpp::lower::Lower::lower_decls` + `Lower::finish`, 是 driver 唯一调用
//! 的 legacy lowering 接口. Stage 1 拆分时, lower.rs 的实现会逐步迁入
//! `expr` / `layout` / `runtime` 子模块, 但本入口的签名必须保持稳定.
//!
//! 注意: 这些是 legacy 路径（C 文本降级），新代码应走 sema → hir →
//! abi_layout → lower_ir 的 native 路径。

pub mod decl_cleanup;
pub mod expr;
pub mod layout;
pub mod name_flatten;
pub mod runtime;
pub mod template_text;
pub mod text_scan;

use std::collections::HashMap;

use crate::cpp::cpp_const_eval::ConstexprFunction;
use crate::cpp::lower::Lower;
use crate::cpp::parser::CppDecl;

/// Legacy lowering 入口: 把 C++ 声明列表降到完整 C 源码字符串.
///
/// 这是 Off / Auto fallback 路径的真实入口 (Stage 0).
///
/// - 接受完整 C++ AST (`&[CppDecl]`) 与 constexpr 函数表, 经
///   `cpp::lower::Lower` 降级.
/// - 输出可直接喂给 dcc C driver 的纯 C 源码字符串.
/// - Stage 1 要求保留 `Lower::lower_decls` 的公共 API, 因此这里只是
///   薄封装, 不复制实现.
///
/// 错误以 `Err(String)` 返回, 让调用方可以走 native fallback 兜底.
pub fn lower_to_c(
    decls: &[CppDecl],
    constexpr_fns: HashMap<String, ConstexprFunction>,
) -> Result<String, String> {
    let mut lower = Lower::with_constexpr_fns(constexpr_fns);
    lower.lower_decls(decls);
    Ok(lower.finish())
}

/// Stage 0: 在 [`lower_to_c`] 输出的末尾拼接 dcc C++ 运行时实现.
///
/// `host cc` 不允许 `-c` 一次编译多个 `.c`; 若把
/// `dccpp_runtime.c` 与 lowering 输出分开传给 cc, 会触发:
///
///   cc: fatal error: cannot specify `-o` with `-c`, `-S` or `-E`
///   with multiple files
///
/// 因此 legacy backend 把 runtime C 源码直接 concat 到 lowering 产物里,
/// 形成单个翻译单元, 与 `-lstdc++` 一起喂给 host cc.
///
/// ## 拼接顺序 (Stage 0 修正)
///
/// 朴素拼接会触发两类冲突:
/// 1. **system header typedef 重定义**: legacy prologue 注入的
///    `typedef int __timer_t;` / `typedef long long intmax_t;` 等与
///    runtime 的 `<bits/types.h>` / `<stdint.h>` 冲突.
/// 2. **`extern` vs `static` 函数冲突**: legacy prologue 声明
///    `extern void __cxa_throw(...)`, runtime 用 `static` 实现, C 不
///    允许在同一 TU 内对同一函数混用.
///
/// 修正方案:
/// - 解析 runtime, 提取所有 `#include` 行, 放到产物最前 (让系统头先
///   定义所有标准类型);
/// - runtime body 紧随其后 (提供 `__cxa_*` / `dccpp_*` 实现, 不再
///   用 `static` 限定);
/// - 最后是 legacy 输出, 预先剥离与系统头冲突的 typedef 与 extern
///   `__cxa_*` 声明.
pub fn lower_to_c_with_runtime(
    decls: &[CppDecl],
    constexpr_fns: HashMap<String, ConstexprFunction>,
) -> Result<String, String> {
    let legacy_src = lower_to_c(decls, constexpr_fns)?;
    let runtime = include_str!("../stdlib/runtime/dccpp_runtime.c");

    // 1. 抽取 runtime 的 `#include` 行 (按出现顺序去重), 放到最前.
    let mut seen_includes: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut includes: Vec<&str> = Vec::new();
    let mut runtime_body_lines: Vec<&str> = Vec::new();
    for line in runtime.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("#include") {
            // 按文本去重 (保留首次出现顺序)
            if seen_includes.insert(trimmed.to_string()) {
                includes.push(line);
            }
        } else {
            runtime_body_lines.push(line);
        }
    }

    // 2. 准备 runtime body: 把 `static` 限定的 `__cxa_*` 函数改为
    //    普通 extern 定义, 与 legacy prologue 的 `extern` 声明一致.
    //    这些函数就是 C++ ABI 入口 (Itanium), 必须对外可见.
    let runtime_body = runtime_body_lines.join("\n");
    let runtime_body = rewrite_runtime_cxa_storage(&runtime_body);

    // 3. 准备 legacy 产物: 剥离与系统头冲突的 typedef / extern 声明.
    let legacy_stripped = strip_conflicting_legacy_decls(&legacy_src);

    // 4. 拼接: 系统头 → runtime body → legacy 输出.
    let mut out = String::with_capacity(
        includes.iter().map(|s| s.len()).sum::<usize>()
            + runtime_body.len()
            + legacy_stripped.len()
            + 64,
    );
    out.push_str("/* === dcc C++ runtime: system headers === */\n");
    for inc in &includes {
        out.push_str(inc);
        out.push('\n');
    }
    out.push_str("\n/* === dcc C++ runtime: implementation === */\n");
    out.push_str(&runtime_body);
    out.push_str("\n/* === dcc legacy lowering output (typedefs stripped) === */\n");
    out.push_str(&legacy_stripped);
    Ok(out)
}

/// 把 runtime 中 `static <ret> __cxa_*(...)` 改成 `<ret> __cxa_*(...)`,
/// 让这些 ABI 入口与 legacy prologue 的 `extern` 声明一致.
///
/// 仅处理 `__cxa_*` 前缀的函数 (C++ ABI 入口), 不动 runtime 内部
/// `static` helper (e.g. `static void dccpp_string_grow_long`).
fn rewrite_runtime_cxa_storage(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        let trimmed = line.trim_start();
        let is_cxa_static_def = trimmed.starts_with("static ")
            && trimmed
                .strip_prefix("static ")
                .and_then(|rest| rest.find('(').map(|p| &rest[..p]))
                .is_some_and(|sig| sig.contains("__cxa_"));
        if is_cxa_static_def {
            // 去掉 `static` 前缀, 改为 extern 定义
            out.push_str(line.trim_start().trim_start_matches("static "));
            out.push('\n');
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    // 去掉末尾追加的多余换行
    out.trim_end().to_string()
}

/// 从 legacy lowering 输出里剥离与系统头 / runtime 冲突的声明.
///
/// 1. `typedef ... __u_char;` 等 GCC builtin typedef (系统 <bits/types.h>
///    会重新定义, 类型不一致).
/// 2. `typedef int wchar_t;` / `typedef long unsigned int size_t;` /
///    `typedef long int ptrdiff_t;` / `typedef long long intmax_t;` /
///    `typedef unsigned long long uintmax_t;` (系统 <stddef.h> /
///    <stdint.h> 已定义).
/// 3. `extern void __cxa_throw(...)` 等 ABI 声明 (runtime 提供定义,
///    且已改为 extern, 重复声明无害但会触发 `static` vs `extern`
///    冲突的根因, 此处直接删除).
///
/// 保留:
/// - `typedef int bool;` (在 `#ifndef __cplusplus` 包裹里, 安全)
/// - `typedef float std_float32_t;` / `std_float64_t;` (非标准, 系统头
///   不提供)
/// - `extern void *_Znwm(size_t);` 等 (runtime 不提供这些, 由 libc
///   / libstdc++ 提供, 必须保留)
fn strip_conflicting_legacy_decls(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        let trimmed = line.trim();
        let drop = is_conflicting_legacy_decl(trimmed);
        if drop {
            out.push_str(
                "/* [dcc legacy: stripped to avoid conflict with system headers / runtime] ",
            );
            out.push_str(line);
            out.push_str(" */\n");
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// 判断一行 legacy 声明是否与 system header / runtime 冲突.
///
/// 严格匹配单行声明 (legacy prologue 都是单行 emit).
fn is_conflicting_legacy_decl(line: &str) -> bool {
    if !line.starts_with("typedef ") && !line.starts_with("extern ") {
        return false;
    }

    // 1. GCC builtin __xxx_t 类型 (lower.rs:5565)
    const GCC_BUILTIN_TYPES: &[&str] = &[
        "__u_char",
        "__u_short",
        "__u_int",
        "__u_long",
        "__quad_t",
        "__u_quad_t",
        "__loff_t",
        "__ino_t",
        "__ino64_t",
        "__dev_t",
        "__gid_t",
        "__uid_t",
        "__mode_t",
        "__nlink_t",
        "__blksize_t",
        "__blkcnt_t",
        "__blkcnt64_t",
        "__off_t",
        "__off64_t",
        "__pid_t",
        "__ssize_t",
        "__syscall_slong_t",
        "__syscall_ulong_t",
        "__time_t",
        "__useconds_t",
        "__suseconds_t",
        "__daddr_t",
        "__key_t",
        "__clock_t",
        "__clockid_t",
        "__timer_t",
        "__id_t",
        "__socklen_t",
        "__intptr_t",
        "__wchar_t",
        "__fpos_t",
        "__fpos64_t",
        "__locale_t",
        // glibc 整数类型 (系统 <stdint.h> 与 <bits/types.h> 已定义)
        "__int8_t",
        "__int16_t",
        "__int32_t",
        "__int64_t",
        "__uint8_t",
        "__uint16_t",
        "__uint32_t",
        "__uint64_t",
        "__caddr_t",
    ];
    for ty in GCC_BUILTIN_TYPES {
        // 匹配 `typedef int __timer_t;` / `typedef unsigned long __timer_t;`
        // 等所有形式.
        if line.contains(&format!("{};", ty)) && line.contains(ty) {
            return true;
        }
    }

    // 2. 标准类型 (lower.rs:8043 strip_cpp_keywords prologue 与
    //    9268 intmax_t 注入)
    const STD_TYPEDEFS: &[&str] = &[
        "wchar_t",
        "size_t",
        "ptrdiff_t",
        "intmax_t",
        "uintmax_t",
        // C99 标准整数类型 (系统 <stdint.h> 已定义)
        "int8_t",
        "int16_t",
        "int32_t",
        "int64_t",
        "uint8_t",
        "uint16_t",
        "uint32_t",
        "uint64_t",
        // POSIX 常用类型 (系统 <sys/types.h> 已定义)
        "off_t",
        "ssize_t",
        "va_list",
        "__gnuc_va_list",
    ];
    for ty in STD_TYPEDEFS {
        // 形如 `typedef int wchar_t;` / `typedef long unsigned int size_t;`
        if line.starts_with("typedef ") && line.contains(&format!(" {};", ty)) {
            // 排除 `typedef int bool;` 等非冲突, 这里只针对白名单类型
            return true;
        }
    }

    // 3. `extern ... __cxa_*;` ABI 声明 (runtime 已提供定义)
    if line.starts_with("extern ") && line.contains("__cxa_") && line.ends_with(';') {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lower_to_c_smoke_plain_class() {
        let src = b"class Counter { public: Counter() { } };";
        let mut p = crate::cpp::parser::CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let constexpr_fns = p.into_constexpr_fns();
        let c = lower_to_c(&decls, constexpr_fns).unwrap();
        assert!(c.contains("struct Counter"), "got: {c}");
        assert!(c.contains("_ZN7CounterC1Ev"), "got: {c}");
    }

    #[test]
    fn lower_to_c_with_runtime_inlines_runtime() {
        let src = b"class Counter { public: Counter() { } };";
        let mut p = crate::cpp::parser::CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let constexpr_fns = p.into_constexpr_fns();
        let c = lower_to_c_with_runtime(&decls, constexpr_fns).unwrap();
        assert!(c.contains("struct Counter"), "got: {c}");
        // runtime 必须被 inlined 进来
        assert!(
            c.contains("__cxa_throw") || c.contains("dccpp_runtime"),
            "runtime not inlined; got head: {}",
            &c[..c.len().min(400)]
        );
    }

    #[test]
    fn dump_lower_to_c_output_for_inspection() {
        let src = b"struct Counter { Counter() : v(0) {} int v; };\n\
                    int main() { Counter c; return c.v; }\n";
        let mut p = crate::cpp::parser::CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let constexpr_fns = p.into_constexpr_fns();
        let c = lower_to_c(&decls, constexpr_fns).unwrap();
        eprintln!("==begin lower_to_c==\n{}\n==end==", c);
    }
}
