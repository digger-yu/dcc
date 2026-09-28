//! Legacy 模板文本实例化: 模板定义剥离与 `X<Args>` 用法的字符串级替换。
//!
//! Stage 1 拆分: 从 `lower.rs` 原样搬来, 行为不变。这是 Stage 8 (模板迁入 sema 的两阶段
//! 查找与真实例化) 之前唯一的模板路径, 也是 Stage 10 的待删除对象。

use crate::cpp::legacy::{
    decl_cleanup::*, expr::*, layout::*, name_flatten::*, runtime::*, text_scan::*,
};

/// 删除模板定义: template <...> struct/class/func ... { ... }
/// C 不支持模板, 需要完全删除
pub(crate) fn remove_template_definitions(s: &str) -> String {
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
        // 检测 template 关键字
        if match_word_at(bytes, i, "template") {
            let mut j = skip_ws(bytes, i + 8);
            // 跳过 <...> 模板参数列表
            if j < len && bytes[j] == b'<' {
                let mut depth = 1i32;
                j += 1;
                while j < len && depth > 0 {
                    if bytes[j] == b'<' {
                        depth += 1;
                    } else if bytes[j] == b'>' {
                        depth -= 1;
                    }
                    j += 1;
                }
                j = skip_ws(bytes, j);
                // 现在 j 指向 template 声明的主体 (struct/class/函数)
                // 跳过 struct/class 关键字
                if match_word_at(bytes, j, "struct") || match_word_at(bytes, j, "class") {
                    let kw_len = if match_word_at(bytes, j, "struct") { 6 } else { 5 };
                    j = skip_ws(bytes, j + kw_len);
                    // 跳过模板类名
                    if j < len && is_ident_char(bytes[j]) {
                        let (_, name_end) = read_ident(bytes, j);
                        j = name_end;
                    }
                    // 跳过可能的模板特化 <...>
                    j = skip_ws(bytes, j);
                    if j < len && bytes[j] == b'<' {
                        let mut depth = 1i32;
                        j += 1;
                        while j < len && depth > 0 {
                            if bytes[j] == b'<' {
                                depth += 1;
                            } else if bytes[j] == b'>' {
                                depth -= 1;
                            }
                            j += 1;
                        }
                    }
                    // 跳过可能的基类列表和 { ... }
                    // 找到 '{' 并跳过整个类定义
                    while j < len && bytes[j] != b'{' && bytes[j] != b';' {
                        j += 1;
                    }
                    if j < len && bytes[j] == b'{' {
                        let mut depth = 1i32;
                        j += 1;
                        while j < len && depth > 0 {
                            if bytes[j] == b'{' {
                                depth += 1;
                            } else if bytes[j] == b'}' {
                                depth -= 1;
                            }
                            j += 1;
                        }
                        // 跳过结尾的 ';'
                        if j < len && bytes[j] == b';' {
                            j += 1;
                        }
                    } else if j < len && bytes[j] == b';' {
                        j += 1;
                    }
                    i = j;
                    continue;
                }
                // 对于模板函数, 跳过整个函数定义
                // 找到 '{' 并跳过整个函数体
                while j < len && bytes[j] != b'{' && bytes[j] != b';' {
                    j += 1;
                }
                if j < len && bytes[j] == b'{' {
                    let mut depth = 1i32;
                    j += 1;
                    while j < len && depth > 0 {
                        if bytes[j] == b'{' {
                            depth += 1;
                        } else if bytes[j] == b'}' {
                            depth -= 1;
                        }
                        j += 1;
                    }
                } else if j < len && bytes[j] == b';' {
                    j += 1;
                }
                i = j;
                continue;
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// Replace template usage patterns: template_name<args> → void*
/// Matches: ident<...> where content looks like template args
/// (contains types, commas, numbers) and is followed by a variable
/// name or is in a type position.
pub(crate) fn replace_template_usages(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        // Skip strings and comments
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
        // Check for identifier followed by '<'
        if is_ident_char(bytes[i]) && (i == 0 || !is_ident_char(bytes[i - 1])) {
            let (ident, ident_end) = read_ident(bytes, i);
            let j = skip_ws(bytes, ident_end);
            if j < len && bytes[j] == b'<' {
                // Skip if this is '<<' (bit-shift operator, not template)
                if j + 1 < len && bytes[j + 1] == b'<' {
                    result.push_str(&ident);
                    i = ident_end;
                    continue;
                }
                // Skip if preceded by '->' or '.' (member access, not template)
                let before_ident = if i > 0 { i - 1 } else { 0 };
                let mut k = before_ident;
                while k > 0 && (bytes[k] == b' ' || bytes[k] == b'\t') {
                    k -= 1;
                }
                if k > 0 && bytes[k] == b'>' && k > 0 && bytes[k - 1] == b'-' {
                    // After ->, this is member access, not template
                    result.push_str(&ident);
                    i = ident_end;
                    continue;
                }
                if k > 0 && bytes[k] == b'.' {
                    // After ., this is member access, not template
                    result.push_str(&ident);
                    i = ident_end;
                    continue;
                }
                // Check if this looks like a template usage
                // Try to find matching '>'
                let mut depth = 1i32;
                let mut k = j + 1;
                let mut _has_comma = false;
                let _has_struct = false;
                let mut has_comparison = false; // Track if content has ==, !=, <=, >=
                let mut _has_parens = false; // Track if content has ( )
                let mut has_semicolon = false;
                let mut has_newline = false;
                let start_k = k;
                while k < len && depth > 0 {
                    // Skip '>>' (bit-shift, not two template closes)
                    if bytes[k] == b'>' && k + 1 < len && bytes[k + 1] == b'>' {
                        depth -= 1;
                        k += 2;
                        continue;
                    }
                    // Skip '<<' (bit-shift, not two template opens)
                    if bytes[k] == b'<' && k + 1 < len && bytes[k + 1] == b'<' {
                        depth += 1;
                        k += 2;
                        continue;
                    }
                    if bytes[k] == b'<' {
                        depth += 1;
                    } else if bytes[k] == b'>' {
                        depth -= 1;
                    } else if bytes[k] == b',' && depth == 1 {
                        _has_comma = true;
                    } else if bytes[k] == b'(' || bytes[k] == b')' {
                        _has_parens = true;
                    } else if bytes[k] == b';' {
                        has_semicolon = true;
                    } else if bytes[k] == b'\n' {
                        has_newline = true;
                    } else if bytes[k] == b';' || bytes[k] == b'{' || bytes[k] == b'}' {
                        break; // Not a template
                    }
                    // Check for comparison operators (==, !=, <=, >=)
                    if k + 1 < len
                        && ((bytes[k] == b'=' && bytes[k + 1] == b'=')
                            || (bytes[k] == b'!' && bytes[k + 1] == b'=')
                            || (bytes[k] == b'<' && bytes[k + 1] == b'=')
                            || (bytes[k] == b'>' && bytes[k + 1] == b'='))
                    {
                        has_comparison = true;
                    }
                    k += 1;
                }
                // Only replace if identifier contains template keywords
                // OR if the content looks like template type arguments
                // (identifiers, types, pointers, references, const, etc.)
                let content_len = k - start_k;
                let looks_like_template = ident.contains("vec")
                    || ident.contains("map")
                    || ident.contains("hash")
                    || ident.contains("set")
                    || ident.contains("pair")
                    || ident.contains("auto_suppress")
                    || ident.contains("unique_ptr")
                    || ident.contains("shared_ptr")
                    || ident.contains("vector")
                    || ident.contains("list")
                    || ident.contains("deque")
                    || ident.contains("array")
                    || ident.contains("tuple")
                    || ident.contains("optional")
                    || ident.contains("function")
                    || ident.contains("allocator")
                    || ident.contains("node"); // alloc_node, hash_node, etc.
                                               // Check if content looks like template type arguments
                                               // (no operators except *, &, ::, no keywords like if/while/for/return)
                let content = &s[start_k..k];
                let content_looks_like_types = !content.contains("if ")
                    && !content.contains("while ")
                    && !content.contains("for ")
                    && !content.contains("return ")
                    && !content.contains("=")
                    && !content.contains("+")
                    && !content.contains("-")
                    && !content.contains("/")
                    && !content.contains("%")
                    && !content.contains("^")
                    && !content.contains("|")
                    && !content.contains("~")
                    && !content.contains("?")
                    && !content.contains(":");
                // Don't replace if:
                // - Content has comparison operators (likely not a template)
                // - Content is too long (spanning across code)
                // - Content has semicolons (spanning statements)
                // - Content has too many newlines (spanning multiple lines incorrectly)
                let is_valid_template = depth == 0
                    && (looks_like_template || content_looks_like_types)
                    && !has_comparison
                    && !has_semicolon
                    && content_len < 200 // Max 200 chars between < and >
                    && !(has_newline && content_len > 100); // If newlines, keep it short
                if is_valid_template {
                    // Looks like template usage
                    // Check what follows '>' - should be identifier (var name)
                    // or ';' or ',' or ')'
                    let after_gt = skip_ws(bytes, k);
                    let is_type_pos = after_gt < len
                        && (is_ident_char(bytes[after_gt])
                            || bytes[after_gt] == b'*'
                            || bytes[after_gt] == b'&'
                            || bytes[after_gt] == b';'
                            || bytes[after_gt] == b','
                            || bytes[after_gt] == b')'
                            || bytes[after_gt] == b'>');
                    if is_type_pos {
                        result.push_str("void*");
                        i = k;
                        continue;
                    }
                }
            }
            // Not a template usage, output the identifier normally
            result.push_str(&ident);
            i = ident_end;
            continue;
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// Apply additional C++ to C conversions after AST lowering or fallback processing
/// This handles constructs that might have been missed by the main lowering passes
/// 在 `s` 中查找不在块注释内的下一个 `template <` 出现位置.
/// 返回 None 表示没有 (或只剩注释内的) 匹配.
pub(crate) fn find_template_lt_outside_comments(s: &str) -> Option<usize> {
    let mut search_from = 0usize;
    while let Some(rel) = s[search_from..].find("template <") {
        let pos = search_from + rel;
        if !is_inside_block_comment(s, pos) {
            return Some(pos);
        }
        search_from = pos + "template <".len();
    }
    None
}

/// 判断 `pos` 是否落在 `s` 的一个未闭合块注释 `/* ... */` 内部.
pub(crate) fn is_inside_block_comment(s: &str, pos: usize) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0usize;
    let mut in_comment = false;
    while i < bytes.len() && i < pos {
        if !in_comment && i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            in_comment = true;
            i += 2;
        } else if in_comment && i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'/' {
            in_comment = false;
            i += 2;
        } else {
            i += 1;
        }
    }
    in_comment
}

/// Convert constexpr functions used in global initializers to macros.
/// This handles the base64_dec table initialization in files.cc.
pub(crate) fn convert_constexpr_fn_to_macro(s: &str) -> String {
    // Only process if we find the specific pattern
    let fn_pattern = "base64_dec_fn (unsigned char c)";
    if let Some(fn_start) = s.find(fn_pattern) {
        let after_sig = fn_start + fn_pattern.len();
        let bytes = s.as_bytes();
        // Find '{'
        let mut j = after_sig;
        while j < bytes.len() && bytes[j] != b'{' {
            j += 1;
        }
        if j >= bytes.len() {
            return s.to_string();
        }
        let body_start = j + 1;
        // Find matching '}'
        let mut depth = 1i32;
        let mut k = body_start;
        while k < bytes.len() && depth > 0 {
            if bytes[k] == b'{' {
                depth += 1;
            } else if bytes[k] == b'}' {
                depth -= 1;
            }
            k += 1;
        }
        let body_end = k - 1;
        let body = &s[body_start..body_end];
        let body_trimmed = body.trim();
        if body_trimmed.starts_with("return ") && body_trimmed.ends_with(';') {
            let expr = body_trimmed[7..body_trimmed.len() - 1].trim();
            // Build macro definition
            let macro_def = format!("#define base64_dec_fn(c) ({})\n", expr);
            // Find start of return type line
            let mut type_start = fn_start;
            while type_start > 0 && bytes[type_start - 1] != b'\n' {
                type_start -= 1;
            }
            let mut result = String::with_capacity(s.len() + macro_def.len());
            result.push_str(&s[..type_start]);
            result.push_str(&macro_def);
            result.push_str(&s[body_end + 1..]);
            return result;
        }
    }
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// C 后端没有模板: 类模板定义必须被整体删除 (含 `{}` 体), 非模板代码原样保留。
    #[test]
    fn class_template_definition_is_removed() {
        let src = "template <typename T> struct Box { T v; };\nint main() { return 0; }\n";
        let out = remove_template_definitions(src);
        assert!(!out.contains("template"), "{out}");
        assert!(!out.contains("struct Box"), "{out}");
        assert!(out.contains("int main()"), "{out}");
    }

    /// 模板函数同理: 从 `template <` 一直跳到匹配的 `}`。
    #[test]
    fn function_template_definition_is_removed() {
        let src = "template <typename T> T id(T x) { return x; }\nint g;\n";
        let out = remove_template_definitions(src);
        assert!(!out.contains("T id"), "{out}");
        assert!(out.contains("int g;"), "{out}");
    }

    /// 块注释里的 `template <` 不是模板声明, 定位必须跳过它。
    #[test]
    fn template_marker_inside_comment_is_not_a_template() {
        let commented = "/* template <typename T> is a keyword */\nint x;\n";
        assert!(find_template_lt_outside_comments(commented).is_none());
        let real = "// lead\n/* nothing */\ntemplate <typename T> struct Y { int v; };\n";
        let pos = find_template_lt_outside_comments(real).expect("真实模板声明应被定位");
        assert_eq!(&real[pos..pos + 10], "template <");
    }

    #[test]
    fn block_comment_membership_is_positional() {
        let s = "/* a < b */ code";
        assert!(is_inside_block_comment(s, 3));
        assert!(!is_inside_block_comment(s, 14));
    }
}
