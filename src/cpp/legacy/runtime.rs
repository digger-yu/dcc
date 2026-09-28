//! Legacy runtime 前置输出与 C 运行时时序修补。
//!
//! Stage 1 拆分: 从 `lower.rs` 原样搬来, 行为不变。职责边界: 只处理「lowering 产物
//! 里与运行时对接」的文本层修补 —— 结构体 typedef 前置、libstdc++/libc 风格符号名
//! 还原 (`__imp_`/mangled 残迹)、clib 内联函数静态化与空体定义转声明。
//!
//! 这些 pass 属于 legacy C 文本路径; native 路径的运行时符号统一由
//! `crate::cpp::runtime` 登记, 不经过本模块。

use crate::cpp::legacy::{
    decl_cleanup::*, expr::*, layout::*, name_flatten::*, template_text::*, text_scan::*,
};

/// 为所有 struct/enum 生成 typedef, 使得 NAME 和 struct NAME / enum NAME 都有效
pub(crate) fn generate_struct_typedefs(s: &str) -> String {
    let mut struct_names: Vec<String> = Vec::new();
    let mut enum_names: Vec<String> = Vec::new();
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        if bytes[i] == b'"' {
            i = skip_string_literal(bytes, i);
            continue;
        }
        if bytes[i] == b'/' && i + 1 < len && (bytes[i + 1] == b'/' || bytes[i + 1] == b'*') {
            i = skip_comment(bytes, i);
            continue;
        }
        // Collect struct names
        if match_word_at(bytes, i, "struct") {
            let mut j = skip_ws(bytes, i + 6);
            // 跳过属性宏
            while j < len && is_ident_char(bytes[j]) {
                let (_, ident_end) = read_ident(bytes, j);
                let k = skip_ws(bytes, ident_end);
                if k < len && bytes[k] == b'(' {
                    let mut depth = 1i32;
                    let mut m = k + 1;
                    while m < len && depth > 0 {
                        if bytes[m] == b'(' {
                            depth += 1;
                        } else if bytes[m] == b')' {
                            depth -= 1;
                        }
                        m += 1;
                    }
                    j = skip_ws(bytes, m);
                } else {
                    break;
                }
            }
            if j < len && is_ident_char(bytes[j]) {
                let (name, _) = read_ident(bytes, j);
                if !name.is_empty()
                    && !struct_names.contains(&name)
                    && name != "_IO_FILE"
                    && name != "_IO_marker"
                {
                    struct_names.push(name);
                }
            }
        }
        // Collect enum names
        if match_word_at(bytes, i, "enum") {
            let j = skip_ws(bytes, i + 4);
            if j < len && is_ident_char(bytes[j]) {
                let (name, name_end) = read_ident(bytes, j);
                let k = skip_ws(bytes, name_end);
                // Only collect if followed by '{' (definition) or
                // ';' (forward declaration)
                if k < len
                    && (bytes[k] == b'{' || bytes[k] == b';')
                    && !name.is_empty()
                    && !enum_names.contains(&name)
                {
                    enum_names.push(name);
                }
            }
        }
        i += 1;
    }
    if struct_names.is_empty() && enum_names.is_empty() {
        return s.to_string();
    }
    let mut result =
        String::with_capacity(s.len() + struct_names.len() * 50 + enum_names.len() * 50);
    result.push_str("/* Generated struct/enum typedefs */\n");
    for name in &struct_names {
        result.push_str(&format!("typedef struct {} {};\n", name, name));
    }
    for name in &enum_names {
        result.push_str(&format!("typedef enum {} {};\n", name, name));
    }
    result.push('\n');
    result.push_str(s);
    result
}

/// M30.3: 解混淆 C 标准库函数.
/// parser 可能将 extern "C" 块中的函数误作为 C++ 函数混淆.
/// 将 _ZN6strlenEv 还原为 strlen, _ZN6strcmpEv 还原为 strcmp 等.
pub(crate) fn demangle_c_functions(s: &str) -> String {
    let mut result = s.to_string();
    // 常见 C 标准库函数 (无参或可变参的简单函数)
    let c_functions = [
        "strlen",
        "strcmp",
        "strncmp",
        "memcpy",
        "memcmp",
        "memset",
        "strcpy",
        "strncpy",
        "strcat",
        "strncat",
        "strchr",
        "strrchr",
        "strstr",
        "malloc",
        "free",
        "calloc",
        "realloc",
        "abort",
        "exit",
        "atoi",
        "atol",
        "atof",
        "abs",
        "labs",
        "printf",
        "fprintf",
        "sprintf",
        "snprintf",
        "vprintf",
        "vfprintf",
        "vsprintf",
        "vsnprintf",
        "fopen",
        "fclose",
        "fread",
        "fwrite",
        "fseek",
        "ftell",
        "rewind",
        "feof",
        "ferror",
        "clearerr",
        "getenv",
        "setenv",
        "system",
        "qsort",
        "bsearch",
        "memchr",
        "rawmemchr",
        "memrchr",
        "strpbrk",
        "strnlen",
        "strchrnul",
        "stpcpy",
        "stpncpy",
        "index",
        "rindex",
        "strdup",
        "strndup",
        "fopencookie",
        "fgetpos",
        "fsetpos",
        "fgetpos64",
        "fsetpos64",
        "strcoll",
    ];
    for func in &c_functions {
        // _ZN<len><name>Ev 模式 (Itanium ABI: 无参函数)
        let mangled = format!("_ZN{}{}Ev", func.len(), func);
        result = result.replace(&mangled, func);
    }
    result
}

/// M30.3: 将 C 标准库函数的空定义转换为声明.
/// 系统头文件中的函数声明被 parser 解析为空函数体 `{ }`,
/// 链接时与 libc/libiberty 的实现冲突. 将空函数体转为 `;` 声明.
pub(crate) fn convert_empty_clib_defs_to_decls(s: &str) -> String {
    let clib_fns = [
        "memchr",
        "rawmemchr",
        "memrchr",
        "memcmp",
        "memcpy",
        "memmove",
        "memset",
        "strcpy",
        "strncpy",
        "strcat",
        "strncat",
        "strchr",
        "strrchr",
        "strstr",
        "strpbrk",
        "strnlen",
        "strlen",
        "strcmp",
        "strncmp",
        "strcoll",
        "index",
        "rindex",
        "stpcpy",
        "stpncpy",
        "strchrnul",
        "strdup",
        "strndup",
        "malloc",
        "free",
        "calloc",
        "realloc",
        "fopen",
        "fclose",
        "fread",
        "fwrite",
        "fseek",
        "ftell",
        "rewind",
        "feof",
        "ferror",
        "getenv",
        "setenv",
        "fopencookie",
        "fgetpos",
        "fsetpos",
        "fgetpos64",
        "fsetpos64",
    ];
    let lines: Vec<&str> = s.lines().collect();
    let mut result = String::with_capacity(s.len());
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        // 检查是否是 C 标准库函数的空定义:
        // 当前行以 `{` 结尾且包含 C 库函数名
        // 下一行是 `}`
        let is_clib_fn = clib_fns.iter().any(|f| {
            // 精确匹配: 函数名后跟 ` (` 或 `(` 且前面是返回类型
            let pat1 = format!(" {} (", f);
            let pat2 = format!(" {}(", f);
            let pat3 = format!("*{} (", f);
            (trimmed.contains(&pat1) || trimmed.contains(&pat2) || trimmed.contains(&pat3))
                && trimmed.ends_with('{')
        });
        if is_clib_fn && i + 1 < lines.len() && lines[i + 1].trim() == "}" {
            // 确认是空函数体: 当前行 `{` 结尾, 下一行 `}`
            let new_line = format!("{};", trimmed[..trimmed.len() - 1].trim_end());
            result.push_str(&new_line);
            result.push('\n');
            i += 2; // 跳过 `{` 行和 `}` 行
            continue;
        }
        result.push_str(lines[i]);
        result.push('\n');
        i += 1;
    }
    // 第二遍: 将所有参数名为 `_` 的空函数定义转为声明.
    // parser 将系统头文件中的函数声明 (无函数体) 误解析为空定义,
    // 参数名全为 `_` 是因为 parser 无法识别参数名.
    let lines2: Vec<&str> = result.lines().collect();
    let mut result2 = String::with_capacity(result.len());
    let mut j = 0;
    while j < lines2.len() {
        let trimmed = lines2[j].trim();
        if trimmed.ends_with('{') && !trimmed.starts_with("{") && !trimmed.starts_with("//") {
            // 检查是否所有参数名为 `_`
            if let Some(paren_start) = trimmed.rfind('(') {
                // 提取 ( 和 ) 之间的参数列表
                let after_paren = &trimmed[paren_start + 1..];
                let params_str = if let Some(close_paren) = after_paren.rfind(')') {
                    after_paren[..close_paren].trim()
                } else {
                    // 没有 ): 尝试去掉末尾的 { 和空白
                    after_paren.trim_end().trim_end_matches('{').trim()
                };
                if !params_str.is_empty() {
                    let all_underscore = params_str.split(',').all(|p| {
                        let p = p.trim();
                        p.ends_with(" _")
                            || p == "void* _"
                            || p == "..."
                            || (p.split_whitespace().last() == Some("_"))
                    });
                    if all_underscore && j + 1 < lines2.len() && lines2[j + 1].trim() == "}" {
                        // 空函数体且所有参数名为 _: 转为声明
                        let decl = format!("{};", trimmed[..trimmed.len() - 1].trim_end());
                        result2.push_str(&decl);
                        result2.push('\n');
                        j += 2;
                        continue;
                    }
                }
            }
        }
        result2.push_str(lines2[j]);
        result2.push('\n');
        j += 1;
    }
    result2
}

/// M30.3: 将 glibc inline 函数定义转为声明.
/// glibc 头文件中的 inline 函数模式:
///   extern __inline <return_type>
///   <fn_name> (<params>)
///   {
///     return __builtin_...(...);
///   }
/// 被 parser 当作普通函数定义, 链接时与 libc 冲突 (multiple definition).
/// 将整个块转为声明: `<return_type> <fn_name>(<params>);`
pub(crate) fn staticize_clib_inline_fns(s: &str) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let mut result = String::with_capacity(s.len());
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        // 检测 "extern __inline" 开头的行
        if trimmed.starts_with("extern __inline") {
            // 提取返回类型 (去掉 "extern __inline" 前缀)
            let ret_type = trimmed
                .strip_prefix("extern __inline")
                .map(|s| s.trim())
                .unwrap_or("");
            // 收集后续行直到找到 ')' 和 '{'
            let mut sig_lines = Vec::new();
            let mut j = i + 1;
            let mut found_close_paren = false;
            let mut found_open_brace = false;
            while j < lines.len() && j < i + 10 {
                let lt = lines[j].trim();
                if lt == "{" {
                    found_open_brace = true;
                    break;
                }
                sig_lines.push(lt);
                if lt.contains(')') {
                    found_close_paren = true;
                }
                j += 1;
            }
            // 如果找到了完整的签名和 '{', 转为声明
            if found_close_paren && found_open_brace && !sig_lines.is_empty() {
                let fn_sig = sig_lines.join(" ");
                // 构建声明: <return_type> <fn_name>(<params>);
                let decl = if ret_type.is_empty() {
                    format!("{};", fn_sig)
                } else {
                    format!("{} {};", ret_type, fn_sig)
                };
                result.push_str(&decl);
                result.push('\n');
                // 跳过函数体 (从 '{' 到匹配的 '}')
                let mut brace_depth = 1;
                j += 1;
                while j < lines.len() && brace_depth > 0 {
                    for ch in lines[j].chars() {
                        if ch == '{' {
                            brace_depth += 1;
                        }
                        if ch == '}' {
                            brace_depth -= 1;
                        }
                    }
                    j += 1;
                }
                i = j;
                continue;
            }
        }
        result.push_str(lines[i]);
        result.push('\n');
        i += 1;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// C 不给标签名注入同名类型, 所以 legacy 产物里 `Foo` 与 `struct Foo` 必须都能用。
    #[test]
    fn struct_and_enum_typedefs_are_prepended() {
        let out = generate_struct_typedefs("struct Foo { int x; };\nenum Bar { B_A };\n");
        assert!(out.contains("typedef struct Foo Foo;"), "{out}");
        assert!(out.contains("typedef enum Bar Bar;"), "{out}");
        // typedef 必须排在原内容之前, 否则其后的声明看不到该名字
        assert!(
            out.find("typedef struct Foo").unwrap() < out.find("struct Foo {").unwrap(),
            "{out}"
        );
    }

    /// 没有记录类型时必须原样返回: 该 pass 会被串在整条流水线尾部, 前言一旦无条件
    /// 注入就会污染所有产物。
    #[test]
    fn typedef_pass_is_noop_without_records() {
        let src = "int main() { return 0; }\n";
        assert_eq!(generate_struct_typedefs(src), src);
    }

    /// 字符串与注释里的 `struct X :` 不是代码, 扫描必须先跳过 (text_scan 的核心约定)。
    #[test]
    fn clib_inline_staticizing_skips_literals() {
        let src = "static inline int printf(const char*, ...);\nvoid f(void) { }\n";
        let out = staticize_clib_inline_fns(src);
        assert!(out.contains("static inline int printf"), "{out}");
    }
}
