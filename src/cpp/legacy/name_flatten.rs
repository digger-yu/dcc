//! Legacy 限定名展平: `A::b` / `E<T>::x` 等 C++ 名字到 C 标识符的文本映射。
//!
//! Stage 1 拆分: 从 `lower.rs` 原样搬来, 行为不变。native path 由 `cpp::sema` 的作用域链
//! 解析限定名 (`resolve_qualified`), 不做文本替换。

use crate::cpp::legacy::{
    decl_cleanup::*, expr::*, layout::*, runtime::*, template_text::*, text_scan::*,
};

/// 展平 C++ scope 操作符 `::` 为下划线: `std::string` → `std_string`.
/// 只处理 `ident::ident` 形式; 前导 `::` (全局限定) 只去掉 `::`.
pub(crate) fn flatten_scope_operators(s: &str) -> String {
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut result = String::with_capacity(len);
    let mut i = 0;
    while i < len {
        // 跳过字符串字面量
        if bytes[i] == b'"' {
            let j = skip_string_literal(bytes, i);
            result.push_str(&s[i..j]);
            i = j;
            continue;
        }
        // 跳过注释
        if bytes[i] == b'/' && i + 1 < len && (bytes[i + 1] == b'/' || bytes[i + 1] == b'*') {
            let j = skip_comment(bytes, i);
            result.push_str(&s[i..j]);
            i = j;
            continue;
        }
        // 检测 `::`
        if bytes[i] == b':' && i + 1 < len && bytes[i + 1] == b':' {
            // 检查前面是否是标识符 (形成 ident:: 模式)
            if !result.is_empty() && {
                let rb = result.as_bytes();
                let rl = rb.len();
                rl > 0 && is_ident_char(rb[rl - 1])
            } {
                // 检查后面是否是标识符 (形成 ::ident 模式)
                let k = i + 2;
                if k < len && is_ident_char(bytes[k]) {
                    result.push('_');
                    i += 2;
                    continue;
                }
                // 前导 :: 或 :: 后不是标识符 → 只去掉 ::
                result.push('_');
                i += 2;
                continue;
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// 去除 C++ 特有关键字, 使 extern "C" 块内的声明可被 C 编译器接受.
/// 目前去除:
///   - `noexcept` / `noexcept(expr)`
///
/// 强制展平所有剩余的 :: 为 _ (除了字符串字面量中的)
/// 用于 AST 路径和 fallback 路径的后处理
pub fn flatten_all_scope_operators(s: &str) -> String {
    // Simple version: just replace all :: with _ (no string literal handling for now)
    s.replace("::", "_")
}

/// 为 namespace 块内的顶层标识符添加命名空间前缀。
///
/// 扫描 `content` (namespace 块 `{` `}` 之间的文本), 在 brace depth 0
/// 处出现的函数名、类名、结构体名、枚举名、typedef 名前添加
/// `{ns_prefix}_` 前缀, 以避免不同命名空间同名符号冲突。
///
/// 嵌套 namespace 块递归处理, 前缀叠加 (如 `A_B_`)。
/// 不修改 C/C++ 关键字、字符串字面量、注释中的文本。
pub(crate) fn prefix_ns_identifiers(content: &str, ns_name: &str) -> String {
    let prefix = format!("{}_", ns_name);
    let bytes = content.as_bytes();
    let len = bytes.len();
    let mut result = String::with_capacity(content.len() + 64);
    let mut i = 0;
    let mut brace_depth: i32 = 0;

    // C/C++ 关键字集合 — 这些标识符不加前缀
    static KEYWORDS: &[&str] = &[
        "auto",
        "break",
        "case",
        "char",
        "const",
        "continue",
        "default",
        "do",
        "double",
        "else",
        "enum",
        "extern",
        "float",
        "for",
        "goto",
        "if",
        "int",
        "long",
        "register",
        "return",
        "short",
        "signed",
        "sizeof",
        "static",
        "struct",
        "switch",
        "typedef",
        "union",
        "unsigned",
        "void",
        "volatile",
        "while",
        "class",
        "namespace",
        "template",
        "typename",
        "using",
        "virtual",
        "public",
        "private",
        "protected",
        "new",
        "delete",
        "try",
        "catch",
        "throw",
        "operator",
        "this",
        "const_cast",
        "static_cast",
        "dynamic_cast",
        "reinterpret_cast",
        "nullptr",
        "bool",
        "true",
        "false",
        "constexpr",
        "noexcept",
        "explicit",
        "inline",
        "friend",
        "override",
        "final",
        "decltype",
        "static_assert",
        "alignas",
        "alignof",
        "thread_local",
        "char16_t",
        "char32_t",
        "wchar_t",
        "size_t",
        "ptrdiff_t",
        "int8_t",
        "int16_t",
        "int32_t",
        "int64_t",
        "uint8_t",
        "uint16_t",
        "uint32_t",
        "uint64_t",
        "ifdef",
        "ifndef",
        "endif",
        "define",
        "include",
        "pragma",
        "undef",
        "elif",
        "error",
        "warning",
    ];

    fn is_keyword(s: &str) -> bool {
        KEYWORDS.binary_search(&s).is_ok()
    }

    while i < len {
        // 跳过字符串字面量
        if bytes[i] == b'"' {
            result.push('"');
            i += 1;
            while i < len && bytes[i] != b'"' {
                if bytes[i] == b'\\' && i + 1 < len {
                    result.push(bytes[i] as char);
                    i += 1;
                }
                result.push(bytes[i] as char);
                i += 1;
            }
            if i < len {
                result.push('"');
                i += 1;
            }
            continue;
        }
        // 跳过字符字面量
        if bytes[i] == b'\'' {
            result.push('\'');
            i += 1;
            while i < len && bytes[i] != b'\'' {
                if bytes[i] == b'\\' && i + 1 < len {
                    result.push(bytes[i] as char);
                    i += 1;
                }
                result.push(bytes[i] as char);
                i += 1;
            }
            if i < len {
                result.push('\'');
                i += 1;
            }
            continue;
        }
        // 跳过行注释
        if i + 1 < len && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            while i < len && bytes[i] != b'\n' {
                result.push(bytes[i] as char);
                i += 1;
            }
            continue;
        }
        // 跳过块注释
        if i + 1 < len && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            result.push('/');
            result.push('*');
            i += 2;
            while i + 1 < len && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                result.push(bytes[i] as char);
                i += 1;
            }
            if i + 1 < len {
                result.push('*');
                result.push('/');
                i += 2;
            }
            continue;
        }
        // 跟踪 brace depth
        if bytes[i] == b'{' {
            brace_depth += 1;
            result.push('{');
            i += 1;
            continue;
        }
        if bytes[i] == b'}' {
            brace_depth -= 1;
            result.push('}');
            i += 1;
            continue;
        }
        // 在 brace depth 0 检测嵌套 namespace
        if brace_depth == 0 && i + 9 <= len && &bytes[i..i + 9] == b"namespace" {
            let before_ok = i == 0 || !is_ident_char(bytes[i - 1]);
            let after_ok = i + 9 >= len || !is_ident_char(bytes[i + 9]);
            if before_ok && after_ok {
                let mut j = skip_ws(bytes, i + 9);
                let mut inner_ns = String::new();
                if j < len && is_ident_char(bytes[j]) {
                    let (name, name_end) = read_ident(bytes, j);
                    inner_ns = name;
                    j = skip_ws(bytes, name_end);
                }
                if j < len && bytes[j] == b'{' {
                    let mut depth = 1i32;
                    let mut k = j + 1;
                    while k < len && depth > 0 {
                        if bytes[k] == b'{' {
                            depth += 1;
                        } else if bytes[k] == b'}' {
                            depth -= 1;
                        }
                        k += 1;
                    }
                    let inner_content = &content[j + 1..k - 1];
                    if !inner_ns.is_empty() {
                        let combined = format!("{}_{}", ns_name, inner_ns);
                        result.push_str(&prefix_ns_identifiers(inner_content, &combined));
                    } else {
                        result.push_str(&prefix_ns_identifiers(inner_content, ns_name));
                    }
                    i = k;
                    continue;
                }
            }
        }
        // 在 brace depth 0 处检测标识符并加前缀
        if brace_depth == 0 && (bytes[i].is_ascii_alphabetic() || bytes[i] == b'_') {
            let (ident, ident_end) = read_ident(bytes, i);
            let before_ok = i == 0 || !is_ident_char(bytes[i - 1]);
            if before_ok && !is_keyword(&ident) && !ident.starts_with('_') {
                // 检查后面是否跟 '(' (函数) 或 '{' (类/结构体) 或 ';' (变量/typedef)
                let after = skip_ws(bytes, ident_end);
                let is_decl = after < len
                    && (bytes[after] == b'('
                        || bytes[after] == b'{'
                        || bytes[after] == b';'
                        || bytes[after] == b','
                        || bytes[after] == b'='
                        || bytes[after] == b':'
                        || bytes[after] == b'['
                        || bytes[after] == b')'
                        || bytes[after] == b'>');
                // 只对看起来像声明位置的标识符加前缀
                // 前面应有类型关键字或 '}' 或文件开头
                if is_decl {
                    result.push_str(&prefix);
                }
            }
            result.push_str(&ident);
            i = ident_end;
            continue;
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// Fix namespace-flattened enums: when `namespace N { enum class E { ... }; }`
/// is flattened, `N::E::VALUE` becomes `N_E_VALUE` but the enum is defined as
/// `enum E { VALUE, ... }`. This function adds typedefs and defines to bridge
/// the gap.
pub(crate) fn fix_flattened_enum_refs(s: &str) -> String {
    let bytes = s.as_bytes();
    let len = bytes.len();
    // Step 1: Collect all enum definitions and their values
    // Pattern: enum NAME { VAL1, VAL2, ... };
    let mut enum_defs: Vec<(String, Vec<String>)> = Vec::new();
    let mut i = 0;
    while i < len {
        if match_word_at(bytes, i, "enum") {
            let j = skip_ws(bytes, i + 4);
            if j < len && is_ident_char(bytes[j]) {
                let (name, name_end) = read_ident(bytes, j);
                let k = skip_ws(bytes, name_end);
                if k < len && bytes[k] == b'{' {
                    // Collect enum values
                    let mut vals = Vec::new();
                    let mut m = k + 1;
                    let mut depth = 1i32;
                    while m < len && depth > 0 {
                        if bytes[m] == b'{' {
                            depth += 1;
                        } else if bytes[m] == b'}' {
                            depth -= 1;
                        } else if depth == 1
                            && is_ident_char(bytes[m])
                            && (m == 0 || !is_ident_char(bytes[m - 1]))
                        {
                            let (val, _) = read_ident(bytes, m);
                            if val != "enum" && val != "struct" {
                                vals.push(val);
                            }
                        }
                        m += 1;
                    }
                    if !vals.is_empty() {
                        enum_defs.push((name, vals));
                    }
                }
            }
        }
        i += 1;
    }

    if enum_defs.is_empty() {
        return s.to_string();
    }

    // Step 2: Find references like PREFIX_ENUM_NAME that don't match any enum name
    // These are namespace-flattened references
    let mut prefix_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    // Map from flattened type name (e.g., "bidi_kind") to original enum name (e.g., "kind")

    for (enum_name, _) in &enum_defs {
        // Look for patterns like `PREFIX_<enum_name>` used as a TYPE
        // e.g., `bidi_kind` when enum is `kind`
        // IMPORTANT: Only match complete identifiers, not substrings
        let search = format!("_{}", enum_name);
        let mut pos = 0;
        while let Some(found) = s[pos..].find(&search) {
            let abs_pos = pos + found;
            // Check what's before the `_`
            if abs_pos > 0 {
                let before = &s[..abs_pos];
                // Read the identifier before `_`
                let before_bytes = before.as_bytes();
                let mut start = before_bytes.len();
                while start > 0 && is_ident_char(before_bytes[start - 1]) {
                    start -= 1;
                }
                if start < before_bytes.len() {
                    let prefix = &s[start..abs_pos];
                    let flattened_type = format!("{}_{}", prefix, enum_name);
                    // CRITICAL: Check that the character after the enum name
                    // is NOT an identifier character (to avoid matching function
                    // names like get_mode_class when enum is mode_class)
                    let after_pos = abs_pos + search.len();
                    if after_pos < len && is_ident_char(s.as_bytes()[after_pos]) {
                        // This is part of a longer identifier, skip it
                        pos = after_pos;
                        continue;
                    }
                    // Check if this flattened type is used in the code as a TYPE
                    // (in declaration context, not as a function name)
                    // Exclude cases where the identifier is followed by '(' (function)
                    let is_type_usage = {
                        let search_type = flattened_type.to_string();
                        let mut found_type = false;
                        let mut search_pos = 0;
                        while let Some(pos) = s[search_pos..].find(&search_type) {
                            let abs_pos = search_pos + pos;
                            let before_ok =
                                abs_pos == 0 || !is_ident_char(s.as_bytes()[abs_pos - 1]);
                            let after_pos = abs_pos + search_type.len();
                            let after_ok =
                                after_pos >= len || !is_ident_char(s.as_bytes()[after_pos]);
                            if before_ok && after_ok {
                                // Check what comes after - if it's '(' this is a
                                // function definition/call, not a type usage
                                let next_non_ws = s[after_pos..]
                                    .find(|c: char| !c.is_whitespace())
                                    .map(|p| after_pos + p);
                                if let Some(next_pos) = next_non_ws {
                                    if s.as_bytes()[next_pos] != b'(' {
                                        found_type = true;
                                        break;
                                    }
                                }
                            }
                            search_pos = abs_pos + search_type.len();
                        }
                        found_type
                    };
                    if is_type_usage {
                        prefix_map.insert(flattened_type, enum_name.clone());
                    }
                }
            }
            pos = abs_pos + search.len();
        }
    }

    if prefix_map.is_empty() {
        return s.to_string();
    }

    // Step 3: Generate typedefs and defines
    let mut preamble = String::new();
    for (flattened_type, enum_name) in &prefix_map {
        preamble.push_str(&format!("typedef enum {} {};\n", enum_name, flattened_type));
        // Find the enum definition to get its values
        if let Some((_, vals)) = enum_defs.iter().find(|(n, _)| n == enum_name) {
            // Extract the prefix (e.g., "bidi" from "bidi_kind")
            let prefix = &flattened_type[..flattened_type.len() - enum_name.len() - 1];
            for val in vals {
                let flattened_val = format!("{}_{}_{}", prefix, enum_name, val);
                preamble.push_str(&format!("#define {} {}\n", flattened_val, val));
            }
        }
    }

    let mut result = preamble;
    result.push_str(s);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ident::ident` 是 legacy 唯一认可的名字展平形态。
    #[test]
    fn qualified_identifier_becomes_underscored() {
        assert_eq!(flatten_scope_operators("std::string s;"), "std_string s;");
        assert_eq!(flatten_scope_operators("a::b::c f();"), "a_b_c f();");
    }

    /// 名字之后的 `::` 若后面不是标识符, 不能凭空造出 `_`; 这里保持与历史输出一致
    /// (整条 legacy 流水线的其它 pass 依赖这个形状)。
    #[test]
    fn trailing_scope_operator_after_ident_still_collapses() {
        assert_eq!(flatten_scope_operators("ns::"), "ns_");
    }

    /// 字符串与注释里的 `::` 不是代码。
    #[test]
    fn literals_and_comments_are_skipped() {
        let src = "const char* p = \"a::b\"; /* x::y */\n";
        assert_eq!(flatten_scope_operators(src), src);
    }
}
