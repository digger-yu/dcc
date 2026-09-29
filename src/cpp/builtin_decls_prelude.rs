//! C++ 前端「语言级内置符号」的声明前导 (builtin declarations prelude)。
//!
//! # 为什么需要这一层
//!
//! GCC 把一类符号实现成**编译器私有知识**而不是头文件里的声明: `__builtin_va_list`
//! 是个内建 AST 节点 (`__va_list_tag [1]`), `__builtin_bswap16` / `__builtin_expect`
//! / `__builtin_ctz` 这批则是没有原型就能调的内建函数。C 前端能走通是因为
//! `src/parser.rs` / `src/preproc.rs` 为它们开了内建分支 (例如把
//! `__builtin_va_list` 就地展开成 `struct __dcc_sysv_va_list` 的单元素数组)。
//!
//! C++ native 管线 (parser → sema → HIR → lower_ir) 没有这批私有知识, 于是两类
//! 硬失败 (实测 GCC 15.3.0 `libcpp/*.cc` 16 个 TU 全部如此):
//!
//!  - `typedef __builtin_va_list va_list;` (glibc `<stdarg.h>` 原文) 被解析成
//!    `MangleType::Class("__builtin_va_list")`, sema 查不到类符号 →
//!    `unknown class type: __builtin_va_list`;
//!  - `__builtin_bswap16(x)` 之类的内建调用在 C++ 里没有隐式函数声明, 直接
//!    `undeclared identifier: __builtin_bswap16` —— 而它是 glibc `<sys/byteorder.h>`
//!    的 `__bswap_16` 宏展开后的形态, 只要包含系统头文件就必然遇到。
//!
//! # 设计
//!
//! 不在 sema 的 `convert_type` / 名字查找里为这些名字开特例 (那会把编译器知识
//! 散落到各个回退分支), 而是把它们写成**一段普通的 C++ 声明**:
//!
//! - [`prelude_source`] 给出源文本 (内置类型 + 内置函数原型);
//! - [`decls`] 用同一个 C++ 解析器把它变成 `Vec<CppDecl>`;
//! - native 管线在建模前把这批声明拼到 TU 声明序列最前面。
//!
//! 之后类登记、ABI 布局、类型降级、调用点发射全部走既有机制; `__builtin_*` 函数
//! 的**代码生成**本来就在后端按名字拦截 (`src/codegen/*/special/builtins.rs`),
//! 因此这里只需要让名字查找通过, 不重复实现任何语义。
//!
//! # 与后端 ABI 的耦合点 (必须同步修改)
//!
//! va_list 结构体的**名字**与**字段偏移**不是自由的:
//! - `src/parser.rs` 的 C 侧同名定义给出 `gp_offset@0 / fp_offset@4 /` /
//!   `overflow_arg_area@8 / reg_save_area@16`, 大小 24、对齐 8;
//! - `src/ir.rs` 以 `ValueType::Aggregate{ name == "__dcc_sysv_va_list" }` 识别
//!   va_list 对象;
//! - x86_64 / aarch64 / riscv64 后端的 `gen_va_start` / `gen_va_arg` 按同样的
//!   0/4/8/16 偏移读写这四个字段。
//!
//! 内置函数原型的**名字集合**也必须与后端拦截表同源: 声明一个后端不认识的
//! `__builtin_xxx` 会变成链接期的 undefined symbol。单元测试
//! [`layout_matches_c_frontend_definition`] 钉死前三处一致性,
//! [`every_declared_builtin_is_emitted_by_some_backend`] 钉死后端拦截集合。

use crate::cpp::parser::CppDecl;
use crate::cpp::parser::CppParser;

/// va_list 底层结构体的名字 —— C 前端、typed IR 与各后端共用的判据。
pub const VA_LIST_STRUCT_NAME: &str = "__dcc_sysv_va_list";

/// GCC 内建 va_list 类型名。
pub const VA_LIST_TYPE_NAME: &str = "__builtin_va_list";

/// [`VA_LIST_TYPE_NAME`] 的别名, glibc `<stdarg.h>` 里 `__gnuc_va_list` 由它派生。
pub const GNUC_VA_LIST_TYPE_NAME: &str = "__gnuc_va_list";

/// `__builtin_va_start` 等变参内建函数的名字。
///
/// 它们同样是编译器内建知识: 源码里 `va_start(ap, last)` 经预处理展开成
/// `__builtin_va_start(ap, last)`, 而 C++ 没有隐式函数声明, 未声明即
/// `undeclared identifier`。各 x86_64/aarch64/riscv64 后端都按**名字**拦截这些
/// 调用并就地生成取参序列, 所以这里只需要声明出让 sema 的名字查找通过。
pub const VA_BUILTIN_FUNCS: &[&str] =
    &["__builtin_va_start", "__builtin_va_end", "__builtin_va_copy", "__builtin_va_arg"];

/// GCC 内建函数的 (名字, 原型) 表 —— **单一真源**。
///
/// 入选条件 (两条都要满足, 由单测把守第二条):
///  1. 它是编译器内建知识而不是库函数: 系统头 (`<sys/byteorder.h>` 的
///     `__bswap_16`、`<stdio.h>` 的 `__attribute__ ((__format_arg__))` 周圈
///     宏、libstdc++ 的 `__builtin_is_constant_evaluated`) 或 GCC 自身源码会直接
///     调用它;
///  2. 至少一个后端的 `special/builtins.rs` 按名字拦截它 —— 否则发个原型只会把
///     一个根本不支持的调用变成链接期的 undefined symbol, 比前端直接报错更难查。
///
/// 原型里参数的**类型不参与语义** (调用处本来就没有严格类型检查, 后端拿到的是
/// 已降级好的操作数), 但**返回类型决定表达式结果的宽度记账**, 因此这里照搬
/// GCC 文档的签名 (`__builtin_expect` 返 `long`, `__builtin_clz` 返 `int`,
/// `__builtin_bswap16` 收/返 `unsigned short` ...), 而不是统一写成 `int` ——
/// 后者会把 `__builtin_expect (sizeof(x), 1)` 这类长整型链降格成 32 位。
pub const BUILTIN_FUNC_DECLS: &[(&str, &str)] = &[
    // ===== 字节序 / 位变换 =====
    ("__builtin_bswap16", "unsigned short __builtin_bswap16 (unsigned short);"),
    ("__builtin_bswap32", "unsigned int __builtin_bswap32 (unsigned int);"),
    ("__builtin_bswap64", "unsigned long __builtin_bswap64 (unsigned long);"),
    ("__builtin_bitreverse8", "unsigned char __builtin_bitreverse8 (unsigned char);"),
    ("__builtin_bitreverse16", "unsigned short __builtin_bitreverse16 (unsigned short);"),
    ("__builtin_bitreverse32", "unsigned int __builtin_bitreverse32 (unsigned int);"),
    ("__builtin_bitreverse64", "unsigned long __builtin_bitreverse64 (unsigned long);"),
    ("__builtin_rotateleft8", "unsigned char __builtin_rotateleft8 (unsigned char, unsigned char);"),
    ("__builtin_rotateleft16", "unsigned short __builtin_rotateleft16 (unsigned short, unsigned short);"),
    ("__builtin_rotateleft32", "unsigned int __builtin_rotateleft32 (unsigned int, unsigned int);"),
    ("__builtin_rotateleft64", "unsigned long __builtin_rotateleft64 (unsigned long, unsigned long);"),
    ("__builtin_rotateright8", "unsigned char __builtin_rotateright8 (unsigned char, unsigned char);"),
    ("__builtin_rotateright16", "unsigned short __builtin_rotateright16 (unsigned short, unsigned short);"),
    ("__builtin_rotateright32", "unsigned int __builtin_rotateright32 (unsigned int, unsigned int);"),
    ("__builtin_rotateright64", "unsigned long __builtin_rotateright64 (unsigned long, unsigned long);"),
    // ===== 位计数 ([gcc] Other Builtins): 结果一律 int =====
    ("__builtin_clz", "int __builtin_clz (unsigned int);"),
    ("__builtin_clzl", "int __builtin_clzl (unsigned long);"),
    ("__builtin_clzll", "int __builtin_clzll (unsigned long);"),
    ("__builtin_ctz", "int __builtin_ctz (unsigned int);"),
    ("__builtin_ctzl", "int __builtin_ctzl (unsigned long);"),
    ("__builtin_ctzll", "int __builtin_ctzll (unsigned long);"),
    ("__builtin_ffs", "int __builtin_ffs (int);"),
    ("__builtin_ffsl", "int __builtin_ffsl (long);"),
    ("__builtin_ffsll", "int __builtin_ffsll (long long);"),
    ("__builtin_popcount", "int __builtin_popcount (unsigned int);"),
    ("__builtin_popcountl", "int __builtin_popcountl (unsigned long);"),
    ("__builtin_popcountll", "int __builtin_popcountll (unsigned long long);"),
    ("__builtin_parity", "int __builtin_parity (unsigned int);"),
    ("__builtin_parityl", "int __builtin_parityl (unsigned long);"),
    ("__builtin_parityll", "int __builtin_parityll (unsigned long long);"),
    ("__builtin_clrsb", "int __builtin_clrsb (int);"),
    ("__builtin_clrsbl", "int __builtin_clrsbl (long);"),
    ("__builtin_clrsbll", "int __builtin_clrsbll (long long);"),
    // ===== 内存 / 字符串操作类内建 (后端自己发序列, 不进 libc) =====
    ("__builtin_bcmp", "int __builtin_bcmp (const void *, const void *, unsigned long);"),
    ("__builtin_bcopy", "void __builtin_bcopy (const void *, void *, unsigned long);"),
    ("__builtin_bzero", "void __builtin_bzero (void *, unsigned long);"),
    ("__builtin_mempcpy", "void *__builtin_mempcpy (void *, const void *, unsigned long);"),
    ("__builtin_stpcpy", "char *__builtin_stpcpy (char *, const char *);"),
    ("__builtin_memcpy_inline", "int __builtin_memcpy_inline (void *, const void *, unsigned long);"),
    ("__builtin_memset_inline", "int __builtin_memset_inline (void *, int, unsigned long);"),
    // ===== 控制流 / 优化提示 =====
    ("__builtin_expect", "long __builtin_expect (long, long);"),
    (
        "__builtin_expect_with_probability",
        "long __builtin_expect_with_probability (long, long, double);",
    ),
    ("__builtin_unreachable", "void __builtin_unreachable ();"),
    ("__builtin_trap", "void __builtin_trap ();"),
    ("__builtin_abort", "void __builtin_abort ();"),
    ("__builtin_debugtrap", "void __builtin_debugtrap ();"),
    ("__builtin_prefetch", "void __builtin_prefetch (const void *, ...);"),
    ("__builtin_constant_p", "int __builtin_constant_p (...);"),
    ("__builtin_object_size", "long __builtin_object_size (const void *, ...);"),
    ("__builtin_dynamic_object_size", "long __builtin_dynamic_object_size (const void *, ...);"),
    ("__builtin_choose_expr", "long __builtin_choose_expr (...);"),
    ("__builtin_classify_type", "int __builtin_classify_type (...);"),
    ("__builtin_types_compatible_p", "int __builtin_types_compatible_p (...);"),
    ("__builtin_is_constant_evaluated", "bool __builtin_is_constant_evaluated ();"),
    ("__builtin_frame_address", "void *__builtin_frame_address (unsigned int);"),
    ("__builtin_return_address", "void *__builtin_return_address (unsigned int);"),
    ("__builtin_extract_return_addr", "void *__builtin_extract_return_addr (void *);"),
    // ===== 浮点内建 (libstdc++ <cmath> / <complex> 直接调用) =====
    ("__builtin_fabs", "double __builtin_fabs (double);"),
    ("__builtin_fabsf", "float __builtin_fabsf (float);"),
    ("__builtin_fabsl", "long double __builtin_fabsl (long double);"),
    ("__builtin_sqrt", "double __builtin_sqrt (double);"),
    ("__builtin_sqrtf", "float __builtin_sqrtf (float);"),
    ("__builtin_sqrtl", "long double __builtin_sqrtl (long double);"),
    ("__builtin_floor", "double __builtin_floor (double);"),
    ("__builtin_floorf", "float __builtin_floorf (float);"),
    ("__builtin_floorl", "long double __builtin_floorl (long double);"),
    ("__builtin_ceil", "double __builtin_ceil (double);"),
    ("__builtin_ceilf", "float __builtin_ceilf (float);"),
    ("__builtin_ceill", "long double __builtin_ceill (long double);"),
    ("__builtin_trunc", "double __builtin_trunc (double);"),
    ("__builtin_truncf", "float __builtin_truncf (float);"),
    ("__builtin_truncl", "long double __builtin_truncl (long double);"),
    ("__builtin_round", "double __builtin_round (double);"),
    ("__builtin_roundf", "float __builtin_roundf (float);"),
    ("__builtin_roundl", "long double __builtin_roundl (long double);"),
    ("__builtin_pow", "double __builtin_pow (double, double);"),
    ("__builtin_powf", "float __builtin_powf (float, float);"),
    ("__builtin_powl", "long double __builtin_powl (long double, long double);"),
    ("__builtin_fmin", "double __builtin_fmin (double, double);"),
    ("__builtin_fminf", "float __builtin_fminf (float, float);"),
    ("__builtin_fminl", "long double __builtin_fminl (long double, long double);"),
    ("__builtin_fmax", "double __builtin_fmax (double, double);"),
    ("__builtin_fmaxf", "float __builtin_fmaxf (float, float);"),
    ("__builtin_fmaxl", "long double __builtin_fmaxl (long double, long double);"),
    ("__builtin_copysign", "double __builtin_copysign (double, double);"),
    ("__builtin_copysignf", "float __builtin_copysignf (float, float);"),
    ("__builtin_copysignl", "long double __builtin_copysignl (long double, long double);"),
    ("__builtin_fma", "double __builtin_fma (double, double, double);"),
    ("__builtin_fmaf", "float __builtin_fmaf (float, float, float);"),
    ("__builtin_fmal", "long double __builtin_fmal (long double, long double, long double);"),
    ("__builtin_signbit", "int __builtin_signbit (double);"),
    ("__builtin_signbitf", "int __builtin_signbitf (float);"),
    ("__builtin_signbitl", "int __builtin_signbitl (long double);"),
    ("__builtin_isnan", "int __builtin_isnan (double);"),
    ("__builtin_isfinite", "int __builtin_isfinite (double);"),
    ("__builtin_isinf", "int __builtin_isinf (double);"),
    ("__builtin_add_overflow", "bool __builtin_add_overflow (...);"),
    ("__builtin_sub_overflow", "bool __builtin_sub_overflow (...);"),
    ("__builtin_mul_overflow", "bool __builtin_mul_overflow (...);"),
    ("__builtin_abs", "int __builtin_abs (int);"),
    ("__builtin_labs", "long __builtin_labs (long);"),
    ("__builtin_llabs", "long long __builtin_llabs (long long);"),
];

/// 本表里的全部内建函数名 (供与后端拦截表做一致性校验)。
pub fn builtin_func_names() -> impl Iterator<Item = &'static str> {
    BUILTIN_FUNC_DECLS.iter().map(|(name, _)| *name)
}

/// 该名字是否是 va_list / 变参内建符号 (供错误信息与特性判定使用)。
pub fn is_va_builtin_name(name: &str) -> bool {
    name == VA_LIST_TYPE_NAME
        || name == GNUC_VA_LIST_TYPE_NAME
        || name == VA_LIST_STRUCT_NAME
        || VA_BUILTIN_FUNCS.contains(&name)
}

/// 内置符号前导的源文本: 内置类型 + [`BUILTIN_FUNC_DECLS`] 里的全部原型。
///
/// `__builtin_va_list` 必须是**单元素数组**而不是裸结构体: SysV AMD64 规定
/// `va_list` 作为函数形参时按指针传递 (数组退化), `va_start` 在调用者帧里则是
/// 24 字节对象。C 前端用 `ArraySize::Const(1)` 表达同一件事, 这里用
/// `typedef <struct> __builtin_va_list[1]` 走 C++ 自己的数组 typedef 通道。
pub fn prelude_source() -> String {
    let mut s = String::from(
        "\
struct __dcc_sysv_va_list {
    unsigned int gp_offset;
    unsigned int fp_offset;
    void *overflow_arg_area;
    void *reg_save_area;
};
typedef __dcc_sysv_va_list __builtin_va_list[1];
typedef __builtin_va_list __gnuc_va_list;
void __builtin_va_start(__builtin_va_list, ...);
void __builtin_va_end(__builtin_va_list);
void __builtin_va_copy(__builtin_va_list, __builtin_va_list);
",
    );
    for (_, decl) in BUILTIN_FUNC_DECLS {
        s.push_str(decl);
        s.push('\n');
    }
    s
}

/// 把 [`prelude_source`] 解析成声明序列, 供 native 管线拼接。
///
/// 解析失败是编译器自身的 bug (前导文本是本模块的常量), 因此这里不做降级:
/// 返回 `None` 时调用方保持原有行为, 但对应的单元测试会立刻失败。
pub fn decls() -> Option<Vec<CppDecl>> {
    let src = prelude_source();
    let mut parser = CppParser::new(src.as_bytes(), "<builtin-decls-prelude>");
    parser.parse_top_level().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 前导必须能被 C++ 解析器完整吃下 (一条 Opaque 都不允许), 否则"注入内置
    /// 类型"这件事本身就是静默失败的。
    #[test]
    fn prelude_parses_without_opaque_decls() {
        let decls = decls().expect("内置类型前导解析失败");
        assert!(
            !decls.iter().any(|d| matches!(d, CppDecl::Opaque(_))),
            "前导里出现了未识别的声明: {:?}",
            decls
        );
        // struct + 2 个 typedef 数组/别名 + 3 个函数声明
        assert!(decls.len() >= 6, "前导声明数异常: {}", decls.len());
    }

    /// 前导的字段名与顺序必须与 C 前端 `src/parser.rs` 的内联定义逐字一致 ——
    /// 后端 `gen_va_start` 是按 0/4/8/16 的硬编码偏移写这四个字段的。
    #[test]
    fn layout_matches_c_frontend_definition() {
        let c_side = include_str!("../parser.rs");
        // C 侧定义的结构体名与四个字段
        assert!(c_side.contains(VA_LIST_STRUCT_NAME), "C 前端已不再使用 {VA_LIST_STRUCT_NAME}");
        for field in ["gp_offset", "fp_offset", "overflow_arg_area", "reg_save_area"] {
            assert!(prelude_source().contains(field), "前导缺少字段 {field}");
            assert!(c_side.contains(field), "C 前端缺少字段 {field}");
        }
        // typed IR 的判据名
        assert!(
            include_str!("../ir.rs").contains(VA_LIST_STRUCT_NAME),
            "ir.rs 的 va_list 判据已改名, 必须同步"
        );
    }

    #[test]
    fn va_builtin_names_are_recognized() {
        assert!(is_va_builtin_name("__builtin_va_list"));
        assert!(is_va_builtin_name("__builtin_va_arg"));
        assert!(is_va_builtin_name("__gnuc_va_list"));
        assert!(!is_va_builtin_name("printf"));
    }

    /// 内部类型名单必须与实际前导文本里的结构体名一致, 否则一致性校验会
    /// 静默失效 (名字改了但常量还查旧名)。
    #[test]
    fn prelude_names_agree_with_their_own_source_text() {
        let src = prelude_source();
        assert!(src.contains(VA_LIST_STRUCT_NAME), "前导缺少 {VA_LIST_STRUCT_NAME}");
        assert!(src.contains(GNUC_VA_LIST_TYPE_NAME), "前导缺少 {GNUC_VA_LIST_TYPE_NAME}");
    }

    /// 表里声明的每一个 `__builtin_xxx` 都必须至少被一个后端按名字拦截。
    ///
    /// 否则 native 路径会把一个「前端认为合法、后端不会特殊处理」的调用降级成
    /// 普通的 `call __builtin_xxx`, 到链接期才爆 undefined symbol —— 比在前端
    /// 直接报 unsupported 难查得多。后端拦截表是字符串形式, 这里直接扫源码。
    #[test]
    fn every_declared_builtin_is_emitted_by_some_backend() {
        let backends: &[&str] = &[
            include_str!("../codegen/x86_64/special/builtins.rs"),
            include_str!("../codegen/aarch64/special/builtins.rs"),
            include_str!("../codegen/riscv64/special/builtins.rs"),
            // 常量折叠与 C 前端共享的内建识别表
            include_str!("../parser.rs"),
            include_str!("../preproc.rs"),
        ];
        let mut uncovered: Vec<&str> = Vec::new();
        for (name, _) in BUILTIN_FUNC_DECLS {
            let needle = format!("\"{name}\"");
            if !backends.iter().any(|src| src.contains(&needle)) {
                uncovered.push(name);
            }
        }
        assert!(
            uncovered.is_empty(),
            "内置函数表里的 {uncovered:?} 没有任何后端按名字拦截, 必须从表中移除或补后端"
        );
    }

    /// 表里的 (名字, 原型) 必须自洽: 原型里的函数名就是第一元。拼错了会得到
    /// 一个「名字查找能命中、调用点却找不到符号」的双头真源。
    #[test]
    fn builtin_table_names_match_their_own_prototypes() {
        for (name, decl) in BUILTIN_FUNC_DECLS {
            assert!(decl.contains(name), "原型 {decl} 与名字 {name} 不一致");
            assert!(decl.trim_end().ends_with(';'), "原型必须以 ; 结尾 (不能带函数体): {decl}");
        }
    }
}
#[test]
fn dump_prelude() {
    let src = crate::cpp::builtin_decls_prelude::prelude_source();
    let mut p = crate::cpp::parser::CppParser::new(src.as_bytes(), "<p>");
    let ds = p.parse_top_level().unwrap();
    for d in &ds {
        if let crate::cpp::parser::CppDecl::Opaque(t) = d { println!("OPAQUE: {t}"); }
    }
    println!("total decls = {}", ds.len());
    for d in &ds {
        if format!("{d:?}").contains("bswap16") {
            println!("KIND: {d:?}");
        }
    }
}
