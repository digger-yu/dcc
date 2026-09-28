//! Legacy 声明区清理: 属性/存储类/重复定义/泄漏语句等顶层文本修补。
//!
//! Stage 1 拆分: 从 `lower.rs` 原样搬来, 行为不变。这些 pass 只关心「lowering 产物能不能
//! 被 C 编译器接受」, 与 C++ 语义无关, 因此不与 `legacy/runtime.rs` 的运行时符号修补混在一起。

use crate::cpp::legacy::{
    expr::*, layout::*, name_flatten::*, runtime::*, template_text::*, text_scan::*,
};

/// 判断一个属性宏的实参是否就是 `weak`。
///
/// `__attribute__` 的规范写法是**双层括号** (`__attribute__((weak))`), 而本模块的两个
/// 剥离 pass 只按「`IDENT(` 之后到配对 `)` 之前」取实参, 于是读到的内容是 `(weak)`,
/// 注释里承诺的「保留 weak 用于多 TU 安全」在规范写法下**从来不成立** —— weak 被静默
/// 丢掉会让同一符号在多个 legacy TU 里重复定义。这里再剥掉可能存在的内层一对括号,
/// 使 `__attribute__((weak))` 与 `__attribute__(weak)` 两种写法都按预期保留;
/// 带额外参数的 `weak, alias("x")` 仍按「非纯 weak」剥离 (与历史一致)。
fn is_weak_attribute(content: &str) -> bool {
    let t = content.trim();
    let inner = t
        .strip_prefix('(')
        .and_then(|x| x.strip_suffix(')'))
        .map(str::trim)
        .unwrap_or(t);
    inner == "weak"
}

/// M30.3: 剥离 __attribute__((...)) 和 __attribute__(...) 属性标记.
/// C 后端不支持 __attribute__ 语法, 需要移除.
/// 但保留 __attribute__((weak)) - 用于多 TU 安全.
pub(crate) fn strip_attributes(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        // 检查 __attribute__
        if i + 13 <= len && &bytes[i..i + 13] == b"__attribute__" {
            let attr_start = i;
            // 跳过 __attribute__
            i += 13;
            // 跳过空格
            while i < len && bytes[i] == b' ' {
                i += 1;
            }
            // 检查 (( 或 (
            if i < len && bytes[i] == b'(' {
                i += 1;
                // 记录内容开始位置
                let content_start = i;
                // 跳过 balanced parens
                let mut depth = 1i32;
                while i < len && depth > 0 {
                    if bytes[i] == b'(' {
                        depth += 1;
                    } else if bytes[i] == b')' {
                        depth -= 1;
                    }
                    i += 1;
                }
                let content_end = i - 1;
                let content = &s[content_start..content_end];
                // 保留 __attribute__((weak))
                if is_weak_attribute(content) {
                    result.push_str(&s[attr_start..i]);
                }
                // 跳过尾部空格
                while i < len && bytes[i] == b' ' {
                    i += 1;
                }
            }
        } else {
            result.push(bytes[i] as char);
            i += 1;
        }
    }
    result
}

/// 剥离 GCC 属性宏: GTY(...), __attribute__((...)), __declspec(...), 等
/// 这些属性在 C 降级中不需要, 且会干扰解析
pub(crate) fn strip_gcc_attributes(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        // 跳过字符串和注释
        if bytes[i] == b'"' {
            let j = skip_string_literal(bytes, i);
            result.push_str(&s[i..j]);
            i = j;
            continue;
        }
        if bytes[i] == b'/' && i + 1 < len && (bytes[i + 1] == b'/' || bytes[i + 1] == b'*') {
            let j = skip_comment(bytes, i);
            result.push_str(&s[i..j]);
            i = j;
            continue;
        }
        // 检测属性宏: IDENT(...) 其中 IDENT 是 GTY, __attribute__, __declspec 等
        if is_ident_char(bytes[i]) && (i == 0 || !is_ident_char(bytes[i - 1])) {
            let (ident, ident_end) = read_ident(bytes, i);
            if ident == "GTY"
                || ident == "__attribute__"
                || ident == "__declspec"
                || ident == "__cdecl"
                || ident == "__stdcall"
                || ident == "__fastcall"
            {
                let j = skip_ws(bytes, ident_end);
                if j < len && bytes[j] == b'(' {
                    // 提取属性内容, 检查是否是 weak
                    let mut depth = 1i32;
                    let mut k = j + 1;
                    let content_start = k;
                    while k < len && depth > 0 {
                        if bytes[k] == b'(' {
                            depth += 1;
                        } else if bytes[k] == b')' {
                            depth -= 1;
                        }
                        k += 1;
                    }
                    let content_end = k - 1;
                    let content = &s[content_start..content_end];
                    // 保留 __attribute__((weak)) - 用于多 TU 安全
                    if ident == "__attribute__" && is_weak_attribute(content) {
                        // 保留整个属性
                        result.push_str(&s[i..k]);
                        i = skip_ws(bytes, k);
                        continue;
                    }
                    // 跳过其他属性宏
                    i = skip_ws(bytes, k);
                    continue;
                }
            }
            // __extension__ 可以单独出现，后面不跟括号
            if ident == "__extension__" {
                i = skip_ws(bytes, ident_end);
                continue;
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// Convert standalone `inline` to `static inline` to avoid C99 inline semantics.
/// In C99, `inline` without `static` has external linkage, causing multiple
/// definition errors when the same inline function appears in multiple TUs.
pub(crate) fn convert_inline_to_static_inline(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        // Skip string literals
        if bytes[i] == b'"' {
            let j = skip_string_literal(bytes, i);
            result.push_str(&s[i..j]);
            i = j;
            continue;
        }
        // Skip comments
        if bytes[i] == b'/' && i + 1 < len && (bytes[i + 1] == b'/' || bytes[i + 1] == b'*') {
            let j = skip_comment(bytes, i);
            result.push_str(&s[i..j]);
            i = j;
            continue;
        }
        // Look for `inline` keyword
        if match_word_at(bytes, i, "inline") {
            // Check if preceded by `static` or `extern`
            let before = result.trim_end();
            let needs_static = !before.ends_with("static")
                && !before.ends_with("extern")
                && !before.ends_with("__inline")
                && !before.ends_with("__inline__");
            if needs_static {
                result.push_str("static inline");
            } else {
                result.push_str("inline");
            }
            i += 6;
            continue;
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// M30.3: 将 std_* 函数定义转为 static inline.
/// C++ 标准库头文件中的 inline 函数 (如 std_strlen, std_memcpy 等)
/// 被 parser 当作普通函数定义, 导致多 TU 链接冲突.
/// 检测这些函数并添加 static inline 前缀.
/// M26: 排除含 _ZN 的 C++ mangled 名函数 (模块导出等), 这些需要
/// 保持外部链接; 只针对 std_ 前缀的函数.
pub(crate) fn staticize_std_functions(s: &str) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let mut result = String::with_capacity(s.len());
    for line in &lines {
        let trimmed = line.trim();
        // 检测 std_* 函数定义 (返回类型 + std_ 前缀函数名)
        // 排除 main 函数 (trampoline 必须是全局的, 否则链接器找不到 main)
        // M26: 排除含 _ZN 的 C++ mangled 名函数 (模块导出需要外部链接)
        let is_main_fn = trimmed.contains("main") && trimmed.contains('(') && trimmed.contains('{');
        let has_cpp_mangled = trimmed.contains("_ZN");
        let is_std_fn = !is_main_fn
            && !has_cpp_mangled
            && (trimmed.starts_with("void ")
                || trimmed.starts_with("void*")
                || trimmed.starts_with("size_t ")
                || trimmed.starts_with("int ")
                || trimmed.starts_with("char ")
                || trimmed.starts_with("char*")
                || trimmed.starts_with("long ")
                || trimmed.starts_with("long*")
                || trimmed.starts_with("double ")
                || trimmed.starts_with("unsigned ")
                || trimmed.starts_with("bool ")
                || trimmed.starts_with("_Bool ")
                || trimmed.starts_with("type_p ")
                || trimmed.starts_with("const_type_p ")
                || trimmed.starts_with("pair_p ")
                || trimmed.starts_with("options_p ")
                || trimmed.starts_with("outf_p ")
                || trimmed.starts_with("input_file*")
                || trimmed.starts_with("lang_bitmap ")
                || trimmed.starts_with("struct fileloc*")
                || trimmed.starts_with("YY_BUFFER_STATE "))
            && trimmed.contains("std_")
            && trimmed.contains('(')
            && !trimmed.starts_with("static")
            && !trimmed.starts_with("extern")
            && !trimmed.ends_with(';');
        if is_std_fn {
            result.push_str("static inline ");
            result.push_str(line.trim_start());
            result.push('\n');
        } else {
            result.push_str(line);
            result.push('\n');
        }
    }
    result
}

/// M30.3: 将 _sch_* 全局变量定义转为 extern 声明.
/// safe-ctype.h 中的 `extern const unsigned short _sch_istable[256]` 等
/// 被 parser 当作定义 `unsigned short _sch_istable;`, 导致多 TU 链接冲突.
/// 转为 extern 声明.
pub(crate) fn fix_sch_extern_decls(s: &str) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let mut result = String::with_capacity(s.len());
    for line in &lines {
        let trimmed = line.trim();
        // 检测 _sch_* 变量定义: "unsigned short _sch_istable;" 等
        if (trimmed.starts_with("unsigned short _sch_")
            || trimmed.starts_with("unsigned char _sch_"))
            && trimmed.ends_with(';')
            && !trimmed.contains('[')
        // 数组声明保持原样
        {
            // 转为 extern 声明
            result.push_str("extern ");
            result.push_str(trimmed);
            result.push('\n');
        } else {
            result.push_str(line);
            result.push('\n');
        }
    }
    result
}

/// M30.3: 将 gengtype 全局变量定义转为 extern 声明.
/// gengtype.h 中声明的全局变量 (如 gt_files, num_gt_files 等)
/// 被 parser 当作定义发射, 导致多 TU 链接冲突.
/// 转为 extern 声明, 实际定义应在一个 TU 中.
pub(crate) fn fix_gengtype_global_vars(s: &str) -> String {
    // gengtype 全局变量列表
    let gengtype_vars = [
        "gt_files",
        "num_gt_files",
        "build_headers",
        "num_build_headers",
        "this_file",
        "system_h_file",
        "lang_dir_names",
        "num_lang_dirs",
        "lexer_toplevel_done",
        "lexer_line",
        "typedefs",
        "structures",
        "variables",
        "string_type",
        "scalar_nonchar",
        "scalar_char",
        "callback_type",
        "output_files",
        "header_file",
        "hit_error",
        "verbosity_level",
        "do_dump",
        "do_debug",
    ];
    let lines: Vec<&str> = s.lines().collect();
    let mut result = String::with_capacity(s.len());
    for line in &lines {
        let trimmed = line.trim();
        // 检查是否是 gengtype 全局变量定义 (无 extern, 无初始化器)
        let is_gengtype_var = gengtype_vars.iter().any(|v| {
            trimmed.ends_with(&format!(" {};", v))
                || trimmed.ends_with(&format!("* {};", v))
                || trimmed.ends_with(&format!("** {};", v))
        });
        if is_gengtype_var && !trimmed.starts_with("extern") 
            && !trimmed.starts_with("static")
            && !trimmed.contains('=')  // 排除赋值语句 (如 p->next = typedefs;)
            && !trimmed.contains("->") // 排除成员访问
            && !trimmed.contains('.')
        // 排除成员访问
        {
            result.push_str("extern ");
            result.push_str(trimmed);
            result.push('\n');
        } else {
            result.push_str(line);
            result.push('\n');
        }
    }
    result
}

/// M30.3: 清理泄漏到全局作用域的函数体语句.
/// parser 有时会将函数体内的局部变量声明泄漏到全局作用域,
/// 导致 conflicting types 错误. 检测并移除这些泄漏语句.
/// 泄漏语句的特征: 在函数定义之间的连续简单声明行.
pub(crate) fn remove_leaked_fn_body_stmts(s: &str) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let mut result = String::with_capacity(s.len());
    let mut i = 0;
    // M26: 跟踪花括号深度, 只在顶层 `}` (深度回到 0) 后才检测泄漏.
    // 避免将函数体内 if/while 块后的局部变量声明误判为“泄漏语句”.
    let mut brace_depth: i32 = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        // 更新花括号深度
        for ch in trimmed.chars() {
            if ch == '{' {
                brace_depth += 1;
            } else if ch == '}' {
                brace_depth -= 1;
            }
        }
        // 检查是否是函数/宏定义结束后的泄漏语句块
        // 特征: 前一行是 `}` 且花括号深度回到 0 (顶层函数结束),
        // 当前行是简单声明, 且后续有多行简单声明
        if trimmed == "}" && brace_depth == 0 && i + 1 < lines.len() {
            // 检查下一行开始是否是泄漏的声明块
            let mut leak_start = i + 1;
            // 跳过空行
            while leak_start < lines.len() && lines[leak_start].trim().is_empty() {
                leak_start += 1;
            }
            if leak_start < lines.len() {
                // 检查是否有连续的简单声明行 (泄漏的局部变量)
                let mut leak_end = leak_start;
                let mut has_struct_decl = false;
                while leak_end < lines.len() {
                    let lt = lines[leak_end].trim();
                    if lt.is_empty() {
                        leak_end += 1;
                        continue;
                    }
                    // 检查是否是简单声明: `type name;` 或 `type name = expr;`
                    let is_simple_decl = (lt.ends_with(';')
                        && !lt.starts_with("//")
                        && !lt.starts_with("/*")
                        && !lt.starts_with("#")
                        && !lt.starts_with("static")
                        && !lt.starts_with("extern")
                        && !lt.starts_with("typedef")
                        && !lt.starts_with("void __dcc_")
                        && !lt.contains("{")
                        && !lt.contains("}"))
                        || (lt.starts_with("struct ") && lt.ends_with(';'));
                    if is_simple_decl {
                        if lt.starts_with("struct ") {
                            has_struct_decl = true;
                        }
                        leak_end += 1;
                    } else {
                        break;
                    }
                }
                // 如果检测到泄漏声明块:
                // - 2+ 行且包含 struct 声明, 或
                // - 3+ 行简单声明 (无 struct 也可能是泄漏)
                // 则认为是泄漏的函数体, 移除这些行
                let is_leak =
                    (has_struct_decl && leak_end - leak_start >= 2) || leak_end - leak_start >= 3;
                if is_leak {
                    // 检查泄漏块后是否是函数定义或文件结尾
                    let mut after_leak = leak_end;
                    while after_leak < lines.len() && lines[after_leak].trim().is_empty() {
                        after_leak += 1;
                    }
                    let is_before_fn = after_leak >= lines.len()
                        || lines[after_leak].trim().ends_with('{')
                        || lines[after_leak].starts_with("/* M30.3");
                    if is_before_fn {
                        // 输出 `}` 后跳过泄漏的声明块
                        result.push_str(lines[i]);
                        result.push('\n');
                        i = leak_end;
                        continue;
                    }
                }
            }
        }
        result.push_str(lines[i]);
        result.push('\n');
        i += 1;
    }
    result
}

/// M30.3: 去重全局函数定义.
/// parser 可能将同一函数声明解析为多个定义 (如 snprintf, strtoimax 等),
/// 导致链接时 multiple definition. 保留第一个定义, 后续重复转为声明.
pub(crate) fn dedup_global_fn_defs(s: &str) -> String {
    use std::collections::HashSet;
    let lines: Vec<&str> = s.lines().collect();
    let mut result = String::with_capacity(s.len());
    let mut defined_fns: HashSet<String> = HashSet::new();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        // 检测函数定义模式: "... fn_name (...) {" 或 "... fn_name (...) { body }"
        // 排除: static, extern, typedef, if, while, for, switch, preprocessor
        if !trimmed.is_empty()
            && !trimmed.starts_with('#')
            && !trimmed.starts_with("//")
            && !trimmed.starts_with("/*")
            && !trimmed.starts_with("static")
            && !trimmed.starts_with("extern")
            && !trimmed.starts_with("typedef")
            && !trimmed.starts_with("if")
            && !trimmed.starts_with("while")
            && !trimmed.starts_with("for")
            && !trimmed.starts_with("switch")
            && !trimmed.starts_with("return")
            && !trimmed.starts_with("struct ")
            && !trimmed.starts_with("union ")
            && !trimmed.starts_with("enum ")
        {
            // 查找模式: identifier '(' ... ')' 后面跟 '{'
            if let Some(paren_pos) = trimmed.find('(') {
                // 提取函数名 (括号前的标识符)
                let before_paren = trimmed[..paren_pos].trim();
                if let Some(fn_name) = before_paren
                    .rsplit_once(' ')
                    .or_else(|| before_paren.rsplit_once('*'))
                    .map(|(_, name)| name.trim())
                    .or_else(|| {
                        // 如果没空格, 整个 before_paren 可能是 fn_name
                        if !before_paren.is_empty()
                            && before_paren
                                .chars()
                                .all(|c| c.is_alphanumeric() || c == '_')
                        {
                            Some(before_paren)
                        } else {
                            None
                        }
                    })
                {
                    if !fn_name.is_empty()
                        && fn_name
                            .chars()
                            .next()
                            .map(|c| c.is_alphabetic() || c == '_')
                            .unwrap_or(false)
                    {
                        // 检查行是否包含 '{' (函数定义开始)
                        if trimmed.contains('{') {
                            if defined_fns.contains(fn_name) {
                                // 重复定义 - 转为声明
                                // 提取到 ')' 为止的部分作为声明
                                if let Some(close_paren) = trimmed.rfind(')') {
                                    let decl_part = &trimmed[..=close_paren];
                                    result.push_str(decl_part);
                                    result.push_str(";\n");
                                }
                                // 跳过函数体
                                if trimmed.ends_with('}') {
                                    // 单行函数
                                    i += 1;
                                } else {
                                    // 多行函数 - 跳过到匹配的 '}'
                                    let mut brace_depth: i32 = 0;
                                    for ch in trimmed.chars() {
                                        if ch == '{' {
                                            brace_depth += 1;
                                        }
                                        if ch == '}' {
                                            brace_depth -= 1;
                                        }
                                    }
                                    i += 1;
                                    while i < lines.len() && brace_depth > 0 {
                                        for ch in lines[i].chars() {
                                            if ch == '{' {
                                                brace_depth += 1;
                                            }
                                            if ch == '}' {
                                                brace_depth -= 1;
                                            }
                                        }
                                        i += 1;
                                    }
                                }
                                continue;
                            } else {
                                defined_fns.insert(fn_name.to_string());
                            }
                        }
                    }
                }
            }
        }
        result.push_str(lines[i]);
        result.push('\n');
        i += 1;
    }
    result
}

/// M30.3: 动态全局初始化转换.
/// C++ 允许全局变量用函数调用初始化, C 不允许.
/// 转换为: 声明 `Type var;` + 生成 `__dcc_init_var()` 函数.
/// 注意: 这只是简单文本转换, 复杂情况可能不完整.
pub(crate) fn convert_dynamic_global_inits(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut init_fns = String::new();
    let mut brace_depth: i32 = 0;
    for line in s.lines() {
        // 跟踪花括号深度 (0 = 全局作用域)
        for ch in line.chars() {
            if ch == '{' {
                brace_depth += 1;
            }
            if ch == '}' {
                brace_depth -= 1;
            }
        }
        // 只在全局作用域 (brace_depth == 0) 转换
        // 注意: 上面的循环已更新了 depth, 所以当前行的 depth
        // 可能已经变了. 我们需要行首的 depth.
        // 简化: 只在 depth == 0 且行匹配模式时转换
        let trimmed = line.trim();
        if brace_depth == 0
            && !trimmed.is_empty()
            && !trimmed.starts_with("//")
            && !trimmed.starts_with("/*")
            && !trimmed.starts_with('#')
            && !trimmed.starts_with("static")
            && !trimmed.starts_with("extern")
            && !trimmed.starts_with("typedef")
            && !trimmed.starts_with("union")
            && !trimmed.starts_with("enum")
            && !trimmed.starts_with("void ")
            && !trimmed.starts_with("int ")
            && !trimmed.starts_with("return")
            && !trimmed.starts_with("if")
            && !trimmed.starts_with("for")
            && !trimmed.starts_with("while")
            && !trimmed.starts_with("}")
            && !trimmed.starts_with("{")
            && !trimmed.contains("->")
            && !trimmed.contains("this")
            && trimmed.contains(" = ")
            && trimmed.contains('(')
            && trimmed.ends_with(";")
        {
            // 尝试解析: `Type var = expr;`
            if let Some(eq_pos) = trimmed.find(" = ") {
                let decl_part = &trimmed[..eq_pos];
                let init_part = &trimmed[eq_pos + 3..trimmed.len() - 1];
                // 检查 init 是否包含函数调用 (含有 `(`)
                // 且 decl_part 看起来像类型+变量名 (至少两个单词)
                if init_part.contains('(') && decl_part.split_whitespace().count() >= 2 {
                    let var_name = decl_part.split_whitespace().last().unwrap_or("unknown");
                    // 确保变量名是合法标识符
                    if var_name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                        result.push_str(&format!("{};\n", decl_part));
                        init_fns.push_str(&format!(
                            "static void __dcc_init_{}(void) {{ {} = {}; }}\n",
                            var_name, decl_part, init_part
                        ));
                        continue;
                    }
                }
            }
        }
        result.push_str(line);
        result.push('\n');
    }
    if !init_fns.is_empty() {
        result.push_str("\n/* M30.3: dynamic global initializers */\n");
        result.push_str(&init_fns);
        result.push_str("void __dcc_run_global_inits(void) {\n");
        for line in init_fns.lines() {
            if line.starts_with("static void __dcc_init_") {
                if let Some(name) = line.split("__dcc_init_").nth(1) {
                    if let Some(paren_pos) = name.find('(') {
                        let fn_name = &name[..paren_pos];
                        result.push_str(&format!("    __dcc_init_{}();\n", fn_name));
                    }
                }
            }
        }
        result.push_str("}\n");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 属性宏是 C 后端不认的语法, 必须整体剥掉。
    #[test]
    fn non_weak_attributes_are_stripped() {
        let out = strip_attributes("__attribute__((unused)) static int f(void) { return 0; }");
        assert!(!out.contains("__attribute__"), "{out}");
        assert!(out.contains("static int f"), "{out}");
    }

    /// `weak` 是**必须保留**的例外 (多 TU 链接安全)。规范写法是双层括号,
    /// 历史 pass 只比一层因此从未命中 —— 本用例钉住修复后的两种写法。
    #[test]
    fn weak_attribute_survives_both_spellings() {
        for src in [
            "__attribute__((weak)) int w(void) { return 0; }",
            "__attribute__(weak) int w(void) { return 0; }",
        ] {
            let out = strip_attributes(src);
            assert!(out.contains("weak"), "{src} -> {out}");
            assert!(out.contains("int w"), "{src} -> {out}");
        }
        let out = strip_gcc_attributes("__attribute__((weak)) int w(void) { return 0; }");
        assert!(out.contains("weak"), "{out}");
    }

    /// 带附加参数的 weak 不是「纯 weak」, 仍按历史行为剥离。
    #[test]
    fn weak_with_extra_arguments_is_still_stripped() {
        let out = strip_attributes("__attribute__((weak, alias(\"other\"))) int v;");
        assert!(!out.contains("alias"), "{out}");
    }

    /// 裸 `inline` 在 C99 里是外部链接, 会变成每 TU 一份定义。
    #[test]
    fn bare_inline_gains_static_existing_linkage_untouched() {
        assert_eq!(
            convert_inline_to_static_inline("inline int f(void) { return 0; }"),
            "static inline int f(void) { return 0; }"
        );
        for src in ["static inline int g(void);", "extern inline int h(void);"] {
            assert_eq!(convert_inline_to_static_inline(src), src, "{src}");
        }
    }

    /// 顶层重复定义只留**一份定义**, 后出现的降为声明 (否则 host cc 报 multiple definition,
    /// 而直接删行又会连带删掉函数体后的内容)。
    #[test]
    fn duplicate_top_level_definition_is_deduped() {
        let src = "int f(void) { return 1; }\nint f(void) { return 1; }\n";
        let out = dedup_global_fn_defs(src);
        assert_eq!(out.matches("return 1").count(), 1, "{out}");
        assert!(out.contains("int f(void);"), "{out}");
    }
}
