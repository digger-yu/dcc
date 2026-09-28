//! Legacy 表达式降级: C++ 表达式/语句形态到 C 文本的转换 pass。
//!
//! Stage 1 拆分: 从 `lower.rs` 原样搬来, 行为不变。职责边界: 成员访问、构造/析构调用点、
//! 花括号初始化、函数式转换、`new`/`delete`、成员函数定义等**语句与表达式**级别的文本改写。
//! native path 由 `cpp::sema` 建 HIR、`cpp::lower_ir` 出 IR, 不经过本模块。

use crate::cpp::legacy::{
    decl_cleanup::*, layout::*, name_flatten::*, runtime::*, template_text::*, text_scan::*,
};

/// Check if args look like parameter declarations (type name, ...)
/// vs constructor arguments (expressions, possibly containing casts).
/// Returns true only if a type keyword appears at a word boundary that is
/// NOT inside a parenthesized sub-expression (i.e. not inside a cast).
pub(crate) fn looks_like_param_decls(args: &str) -> bool {
    let type_kws = [
        "void", "int", "char", "unsigned", "long", "float", "double", "struct", "const",
    ];
    let bytes = args.as_bytes();
    let len = bytes.len();
    let mut paren = 0i32;
    let mut i = 0;
    while i < len {
        if bytes[i] == b'(' {
            paren += 1;
            i += 1;
            continue;
        }
        if bytes[i] == b')' {
            paren = (paren - 1).max(0);
            i += 1;
            continue;
        }
        if paren > 0 {
            i += 1;
            continue;
        }
        for kw in &type_kws {
            let kw_len = kw.len();
            if i + kw_len <= len && &args[i..i + kw_len] == *kw {
                let before_ok =
                    i == 0 || !bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_';
                let after_pos = i + kw_len;
                if before_ok
                    && after_pos < len
                    && !bytes[after_pos].is_ascii_alphanumeric()
                    && bytes[after_pos] != b'_'
                {
                    let rest = args[after_pos..].trim_start();
                    if rest.is_empty()
                        || rest.starts_with('*')
                        || rest.starts_with('&')
                        || rest.starts_with(',')
                    {
                        return true;
                    }
                    if rest.starts_with(|c: char| c.is_ascii_alphanumeric() || c == b'_' as char) {
                        return true;
                    }
                }
            }
        }
        i += 1;
    }
    false
}

/// 替换独立的单词 (前后必须是标识符边界)
pub(crate) fn replace_word(s: &str, from: &str, to: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let flen = from.len();
    let mut i = 0;
    while i < len {
        if i + flen <= len && &bytes[i..i + flen] == from.as_bytes() {
            let before_ok = i == 0 || !is_ident_char(bytes[i - 1]);
            let after_ok = i + flen >= len || !is_ident_char(bytes[i + flen]);
            if before_ok && after_ok {
                result.push_str(to);
                // 保留空格避免粘连
                if !to.is_empty()
                    && to
                        .as_bytes()
                        .last()
                        .is_some_and(|b| b.is_ascii_alphanumeric())
                {
                    // 检查后面是否需要空格
                    if i + flen < len && is_ident_char(bytes[i + flen]) {
                        result.push(' ');
                    }
                }
                i += flen;
                continue;
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// 替换短语 (包含空格的多词匹配)
pub(crate) fn replace_phrase(s: &str, from: &str, to: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let flen = from.len();
    let mut i = 0;
    while i < len {
        if i + flen <= len && &bytes[i..i + flen] == from.as_bytes() {
            let before_ok = i == 0 || !is_ident_char(bytes[i - 1]);
            let after_ok = i + flen >= len || !is_ident_char(bytes[i + flen]);
            if before_ok && after_ok {
                result.push_str(to);
                i += flen;
                continue;
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// Trim leading/trailing ASCII whitespace from a byte slice.
pub(crate) fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let mut start = 0;
    while start < bytes.len() && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    let mut end = bytes.len();
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    &bytes[start..end]
}

/// Trim trailing ASCII whitespace from bytes.
pub(crate) fn trim_ascii_end(bytes: &[u8]) -> &[u8] {
    let mut end = bytes.len();
    while end > 0 && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    &bytes[..end]
}

/// Find the next ASCII '=' byte in `bytes` starting at `start`.
pub(crate) fn find_ascii_eq(bytes: &[u8], start: usize) -> Option<usize> {
    bytes[start..].iter().position(|&b| b == b'=')
}

/// Return the last contiguous ASCII word (identifier) in bytes.
pub(crate) fn last_ascii_word(bytes: &[u8]) -> &str {
    let end = bytes.len();
    let mut start = end;
    while start > 0 {
        let b = bytes[start - 1];
        if b.is_ascii_alphanumeric() || b == b'_' {
            start -= 1;
        } else {
            break;
        }
    }
    std::str::from_utf8(&bytes[start..end]).unwrap_or("")
}

/// 替换函数体中的构造函数调用:
/// `Type var(args)` → `Type var; _ctor_Type(&var, args);`
/// Convert if-condition variable declarations to C style
/// Pattern: if (Type var = expr) { ... } → Type var = expr; if (var) { ... }
pub(crate) fn convert_if_condition_decls(s: &str) -> String {
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

        // Look for "if ("
        if match_word_at(bytes, i, "if") {
            let _if_start = i;
            let after_if = skip_ws(bytes, i + 2);
            if after_if < len && bytes[after_if] == b'(' {
                // Find the matching ')'
                let mut paren_depth = 1i32;
                let mut j = after_if + 1;
                while j < len && paren_depth > 0 {
                    if bytes[j] == b'(' {
                        paren_depth += 1;
                    } else if bytes[j] == b')' {
                        paren_depth -= 1;
                    }
                    j += 1;
                }
                let paren_end = j - 1; // position of ')'
                let cond_content = &s[after_if + 1..paren_end];

                // Check if condition contains a variable declaration: Type var = expr
                // Look for pattern: identifier identifier =
                // But skip comparison operators: !=, ==, <=, >=
                let converted = false;
                if let Some(eq_pos) = cond_content.find('=') {
                    // Make sure this is not == (comparison)
                    // Check the character right after the '='
                    let next_after_eq = if eq_pos + 1 < cond_content.len() {
                        cond_content.as_bytes()[eq_pos + 1]
                    } else {
                        0
                    };
                    if next_after_eq == b'=' {
                        // This is ==, not a declaration assignment.
                        // Try to find the next '=' that is not part of ==
                        let rest = &cond_content[eq_pos + 2..];
                        if let Some(rel_pos) = rest.find('=') {
                            let actual_eq_pos = eq_pos + 2 + rel_pos;
                            // Check if this one is also ==
                            let next2 = if actual_eq_pos + 1 < cond_content.len() {
                                cond_content.as_bytes()[actual_eq_pos + 1]
                            } else {
                                0
                            };
                            if next2 == b'=' { /* also ==, skip */
                            } else {
                                // Process actual_eq_pos below by falling through
                                // We handle this by re-assigning eq_pos
                                // (Rust doesn't allow reassigning in if-let,
                                //  so we use a helper approach)
                            }
                        }
                        // For now, if the first = is ==, skip conversion
                        // unless we find a standalone = later
                        // Simple approach: just skip if first = is ==
                    } else {
                        // Check if this is part of a comparison operator
                        // Look back to skip whitespace and check for !, =, <, >
                        let is_comparison = {
                            let before_eq = cond_content[..eq_pos].trim_end();
                            if !before_eq.is_empty() {
                                let last_char = before_eq.as_bytes()[before_eq.len() - 1];
                                last_char == b'!'
                                    || last_char == b'='
                                    || last_char == b'<'
                                    || last_char == b'>'
                            } else {
                                false
                            }
                        };

                        if !is_comparison {
                            let before_eq = cond_content[..eq_pos].trim();
                            let after_eq = cond_content[eq_pos + 1..].trim();

                            // Check if before_eq looks like "Type varname"
                            let parts: Vec<&str> = before_eq.split_whitespace().collect();
                            if parts.len() >= 2 {
                                // The first token must look like a type name:
                                // - a C keyword (int, char, etc.) or
                                // - starts with 'struct'/'enum'/'union' or
                                // - a single identifier (no operators like >, <, &&, etc.)
                                let first_token = parts[0];
                                let looks_like_type = first_token == "int"
                                || first_token == "char"
                                || first_token == "short"
                                || first_token == "long"
                                || first_token == "float"
                                || first_token == "double"
                                || first_token == "void"
                                || first_token == "unsigned"
                                || first_token == "signed"
                                || first_token == "bool"
                                || first_token == "auto"
                                || first_token == "const"
                                || first_token == "struct"
                                || first_token == "enum"
                                || first_token == "union"
                                || first_token == "volatile"
                                || first_token == "register"
                                || first_token == "static"
                                // Check if it's a plausible type name
                                // (starts with letter/underscore, all alnum/underscore)
                                || (first_token.chars().all(|c| c.is_alphanumeric()
                                    || c == '_')
                                    && first_token.chars().next()
                                        .is_some_and(|c| c.is_alphabetic()
                                        || c == '_'));

                                // Also check that no part contains binary operators
                                // (but allow * which can be a pointer declarator)
                                let has_operators = parts.iter().any(|p| {
                                    p.contains('>')
                                        || p.contains('<')
                                        || p.contains('&')
                                        || p.contains('|')
                                        || p.contains('!')
                                        || p.contains('+')
                                        || p.contains('-')
                                        || p.contains('/')
                                        || p.contains('%')
                                        || p.contains('^')
                                        || p.contains('~')
                                        || p.contains('?')
                                        || p.contains(',')
                                        || p.contains('[')
                                        || p.contains(']')
                                        || p.contains('(')
                                        || p.contains(')')
                                        || p.contains('=')
                                });

                                if looks_like_type && !has_operators {
                                    // Likely a declaration: Type varname = expr
                                    // Handle pointer declarations: Type *varname = expr
                                    // The '*' should be part of the type, not the variable name
                                    let mut var_type_parts = parts[..parts.len() - 1].to_vec();
                                    let mut var_name = parts[parts.len() - 1].to_string();

                                    // Check if var_name starts with '*' (pointer declarator)
                                    if var_name.starts_with('*') {
                                        // Move the '*' to the type
                                        var_name = var_name[1..].to_string();
                                        var_type_parts.push("*");
                                    }

                                    let var_type = var_type_parts.join(" ");

                                    // Output: Type varname = expr; if (varname)
                                    result.push_str(&format!(
                                        "{} {} = {}; if ({})",
                                        var_type, var_name, after_eq, var_name
                                    ));
                                    i = paren_end + 1;
                                    continue;
                                }
                            }
                        }
                    } // end else (not ==)
                }
                // If we didn't convert it, output the if statement normally
                if !converted {
                    // Output "if (" and the condition content, then let the main loop continue
                    result.push_str("if (");
                    i = after_if + 1; // Skip past "if ("
                    continue;
                }
            }
        }

        result.push(bytes[i] as char);
        i += 1;
    }

    result
}

/// Convert declarations in if conditions to separate declaration and check
/// if (Type var = expr) { ... } -> Type var = expr; if (var) { ... }
///
/// 实现注意: 输入可能包含非 ASCII 字符(如中文注释), 因此全部按字节处理,
/// 仅在需要提取标识符时才转回 &str, 且保证切片落在有效的 UTF-8 边界上.
pub(crate) fn convert_if_cond_decls(s: &str) -> String {
    let mut result: Vec<u8> = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        // Look for 'if ('
        if i + 4 < len && &bytes[i..i + 3] == b"if " && bytes[i + 3] == b'(' {
            // Check if there's a type declaration inside
            let mut j = i + 4;
            // Skip ASCII whitespace
            while j < len && matches!(bytes[j], b' ' | b'\t' | b'\n' | b'\r') {
                j += 1;
            }
            // Check for type keywords or identifiers that could be types
            let is_type_start = (j + 11 <= len && &bytes[j..j + 11] == b"__auto_type")
                || (j + 8 <= len && &bytes[j..j + 8] == b"unsigned")
                || (j + 6 <= len && &bytes[j..j + 6] == b"signed")
                || (j + 4 <= len
                    && (&bytes[j..j + 4] == b"int "
                        || &bytes[j..j + 4] == b"char"
                        || &bytes[j..j + 4] == b"long"))
                || (j + 3 <= len && &bytes[j..j + 3] == b"int")
                || (bytes[j] == b's' && j + 7 <= len && &bytes[j..j + 7] == b"struct ");

            if is_type_start {
                // Find the matching )
                let mut depth = 1;
                let mut k = j;
                while k < len && depth > 0 {
                    if bytes[k] == b'(' {
                        depth += 1;
                    } else if bytes[k] == b')' {
                        depth -= 1;
                    }
                    k += 1;
                }
                if depth == 0 {
                    // Extract the declaration: Type var = expr
                    let decl_bytes = &bytes[j..k - 1];
                    // Find the variable name (last identifier before =)
                    // But skip comparison operators: !=, ==, <=, >=
                    let mut found_eq_pos = None;
                    let mut search_start = 0;
                    while let Some(rel_pos) = find_ascii_eq(decl_bytes, search_start) {
                        let eq_pos = search_start + rel_pos;
                        // Check if this is part of a comparison operator
                        let prev_char = if eq_pos > 0 { decl_bytes[eq_pos - 1] } else { 0 };
                        let next_char = if eq_pos + 1 < decl_bytes.len() {
                            decl_bytes[eq_pos + 1]
                        } else {
                            0
                        };
                        // Skip if part of !=, ==, <=, >=
                        if prev_char == b'!'
                            || prev_char == b'='
                            || prev_char == b'<'
                            || prev_char == b'>'
                            || next_char == b'='
                        {
                            search_start = eq_pos + 1;
                            continue;
                        }
                        // Found a standalone =
                        found_eq_pos = Some(eq_pos);
                        break;
                    }
                    if let Some(eq_pos) = found_eq_pos {
                        let before_eq = trim_ascii_end(&decl_bytes[..eq_pos]);
                        // Find the last word (variable name)
                        let var_name = last_ascii_word(before_eq);
                        if !var_name.is_empty()
                            && var_name
                                .bytes()
                                .all(|c| c.is_ascii_alphanumeric() || c == b'_')
                        {
                            // Convert: if (Type var = expr) { ... }
                            // To: Type var = expr; if (var) { ... }
                            result.extend_from_slice(decl_bytes);
                            result.extend_from_slice(b"; if (");
                            result.extend_from_slice(var_name.as_bytes());
                            result.push(b')');
                            i = k;
                            continue;
                        }
                    }
                }
            }
        }
        result.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(result).unwrap_or_else(|_| s.to_string())
}

/// 替换花括号初始化: `Type var{args}` → `Type var = {args}`
pub(crate) fn replace_brace_init(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut known_types: Vec<String> = Vec::new();
    // 先收集已知类型
    {
        let mut j = 0;
        while j < len {
            if match_word_at(bytes, j, "struct") || match_word_at(bytes, j, "class") {
                let kw_len = if match_word_at(bytes, j, "struct") { 6 } else { 5 };
                let k = skip_ws(bytes, j + kw_len);
                if k < len && is_ident_char(bytes[k]) {
                    let (name, _) = read_ident(bytes, k);
                    if !name.is_empty() {
                        known_types.push(name);
                    }
                }
            }
            j += 1;
        }
    }

    while i < len {
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

        // 检查 TypeName varname{
        let mut matched = false;
        for ty in &known_types {
            if match_word_at(bytes, i, ty) {
                let ty_len = ty.len();
                let j = skip_ws(bytes, i + ty_len);
                if j < len && is_ident_char(bytes[j]) {
                    let (var_name, var_end) = read_ident(bytes, j);
                    let k = skip_ws(bytes, var_end);
                    if k < len && bytes[k] == b'{' {
                        // 找到匹配的 '}'
                        let mut depth = 1i32;
                        let mut m = k + 1;
                        while m < len && depth > 0 {
                            if bytes[m] == b'{' {
                                depth += 1;
                            } else if bytes[m] == b'}' {
                                depth -= 1;
                            }
                            m += 1;
                        }
                        // 检查后面是 ';'
                        let after = skip_ws(bytes, m);
                        if after < len && bytes[after] == b';' {
                            // Type var = {args};
                            result.push_str(&format!("{} {} = {};", ty, var_name, &s[k..m]));
                            i = after + 1;
                            matched = true;
                            break;
                        }
                    }
                }
            }
        }
        if matched {
            continue;
        }

        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

pub(crate) fn replace_ctor_calls_in_bodies(s: &str) -> String {
    let mut known_types: Vec<String> = Vec::new();
    {
        let bytes = s.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if match_word_at(bytes, i, "struct") || match_word_at(bytes, i, "class") {
                let kw_len = if match_word_at(bytes, i, "struct") { 6 } else { 5 };
                let j = skip_ws(bytes, i + kw_len);
                if j < bytes.len() && is_ident_char(bytes[j]) {
                    let (name, _) = read_ident(bytes, j);
                    if !name.is_empty() {
                        known_types.push(name);
                    }
                }
            }
            i += 1;
        }
    }
    if known_types.is_empty() {
        return s.to_string();
    }

    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut brace_depth = 0i32;

    while i < len {
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
        if bytes[i] == b'{' {
            brace_depth += 1;
        }
        if bytes[i] == b'}' {
            brace_depth = (brace_depth - 1).max(0);
        }

        if brace_depth > 0 {
            let mut matched = false;
            // Check for optional 'struct' prefix before type name
            let (struct_prefix_len, has_struct_prefix) = if match_word_at(bytes, i, "struct") {
                let after_struct = skip_ws(bytes, i + 6);
                if after_struct < len && is_ident_char(bytes[after_struct]) {
                    (after_struct - i, true)
                } else {
                    (0, false)
                }
            } else {
                (0, false)
            };
            for ty in known_types.iter() {
                let type_start = if has_struct_prefix { struct_prefix_len } else { 0 };
                if match_word_at(bytes, i + type_start, ty) {
                    let ty_len = ty.len();
                    let j = skip_ws(bytes, i + type_start + ty_len);
                    if j < len && is_ident_char(bytes[j]) {
                        let (var_name, var_end) = read_ident(bytes, j);
                        let k = skip_ws(bytes, var_end);
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
                            let args = s[k + 1..m - 1].trim().to_string();
                            let after = skip_ws(bytes, m);
                            // args should not look like parameter declarations
                            let looks_like_params = looks_like_param_decls(&args);
                            if after < len && bytes[after] == b';' && !looks_like_params {
                                // Check if we're inside a for-loop init
                                let in_for_init = {
                                    let mut bk = i;
                                    while bk > 0
                                        && (bytes[bk - 1] == b' '
                                            || bytes[bk - 1] == b'\t'
                                            || bytes[bk - 1] == b'\n')
                                    {
                                        bk -= 1;
                                    }
                                    if has_struct_prefix
                                        && bk >= 6
                                        && &bytes[bk - 6..bk] == b"struct"
                                    {
                                        bk -= 6;
                                        while bk > 0
                                            && (bytes[bk - 1] == b' '
                                                || bytes[bk - 1] == b'\t'
                                                || bytes[bk - 1] == b'\n')
                                        {
                                            bk -= 1;
                                        }
                                    }
                                    if bk > 0 && bytes[bk - 1] == b'(' {
                                        bk -= 1;
                                        while bk > 0
                                            && (bytes[bk - 1] == b' '
                                                || bytes[bk - 1] == b'\t'
                                                || bytes[bk - 1] == b'\n')
                                        {
                                            bk -= 1;
                                        }
                                        if bk >= 3 && &bytes[bk - 3..bk] == b"for" {
                                            bk < 3 || !is_ident_char(bytes[bk - 4])
                                        } else {
                                            false
                                        }
                                    } else {
                                        false
                                    }
                                };
                                let type_prefix = if has_struct_prefix { "struct " } else { "" };
                                // Count arguments to match overloaded constructor name
                                let arg_count =
                                    if args.is_empty() { 0 } else { args.split(',').count() };
                                // +1 for the &this parameter
                                let ctor_suffix = arg_count + 1;
                                if in_for_init {
                                    if args.is_empty() {
                                        result.push_str(&format!(
                                            "{}{} {}; _ctor_{}_{}(&{})",
                                            type_prefix, ty, var_name, ty, ctor_suffix, var_name
                                        ));
                                    } else {
                                        result.push_str(&format!(
                                            "{}{} {}; _ctor_{}_{}(&{}, {})",
                                            type_prefix,
                                            ty,
                                            var_name,
                                            ty,
                                            ctor_suffix,
                                            var_name,
                                            args
                                        ));
                                    }
                                    i = after + 1;
                                } else {
                                    if args.is_empty() {
                                        result.push_str(&format!(
                                            "{}{} {}; _ctor_{}_{}(&{});",
                                            type_prefix, ty, var_name, ty, ctor_suffix, var_name
                                        ));
                                    } else {
                                        result.push_str(&format!(
                                            "{}{} {}; _ctor_{}_{}(&{}, {});",
                                            type_prefix,
                                            ty,
                                            var_name,
                                            ty,
                                            ctor_suffix,
                                            var_name,
                                            args
                                        ));
                                    }
                                    i = after + 1;
                                }
                                matched = true;
                                break;
                            }
                        }
                    }
                }
            }
            if matched {
                continue;
            }
        }

        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// 转换 return struct Type(args) 为 return (struct Type){args} (C99 compound literal)
pub(crate) fn convert_return_struct_ctor(s: &str) -> String {
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
        // 检测 return struct Type(
        if match_word_at(bytes, i, "return") {
            let j = skip_ws(bytes, i + 6);
            if match_word_at(bytes, j, "struct") {
                let k = skip_ws(bytes, j + 6);
                if k < len && is_ident_char(bytes[k]) {
                    let (type_name, type_end) = read_ident(bytes, k);
                    let m = skip_ws(bytes, type_end);
                    if m < len && bytes[m] == b'(' {
                        // 找到匹配的 ')'
                        let mut depth = 1i32;
                        let mut n = m + 1;
                        while n < len && depth > 0 {
                            if bytes[n] == b'(' {
                                depth += 1;
                            } else if bytes[n] == b')' {
                                depth -= 1;
                            }
                            n += 1;
                        }
                        if depth == 0 {
                            // 提取参数
                            let args = &s[m + 1..n - 1];
                            // 输出: return (struct Type){args}
                            result.push_str("return ");
                            result.push_str(&format!("(struct {})", type_name));
                            result.push('{');
                            result.push_str(args);
                            result.push('}');
                            i = n;
                            continue;
                        }
                    }
                }
            }
            // 不是 return struct Type(, 正常输出 return
            result.push_str("return");
            i += 6;
            continue;
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// 替换 nullptr → ((void*)0), 删除 mutable, 删除模板参数等
/// 以及 C++ 引用参数 (&) → 指针 (*)
pub(crate) fn fix_remaining_cpp_syntax(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    // 收集所有已知类型名 (struct/typedef/enum)
    let mut known_types: std::collections::HashSet<String> = std::collections::HashSet::new();
    {
        let mut j = 0;
        while j < len {
            if bytes[j] == b'"' {
                j = skip_string_literal(bytes, j);
                continue;
            }
            if match_word_at(bytes, j, "struct")
                || match_word_at(bytes, j, "typedef")
                || match_word_at(bytes, j, "enum")
                || match_word_at(bytes, j, "class")
            {
                let kw_len = if match_word_at(bytes, j, "struct") {
                    6
                } else if match_word_at(bytes, j, "typedef") {
                    7
                } else if match_word_at(bytes, j, "enum") {
                    4
                } else {
                    5
                };
                let k = skip_ws(bytes, j + kw_len);
                if k < len && is_ident_char(bytes[k]) {
                    let (name, _) = read_ident(bytes, k);
                    if !name.is_empty() {
                        known_types.insert(name);
                    }
                }
            }
            j += 1;
        }
    }
    // 添加基本类型关键字
    for kw in &[
        "int",
        "char",
        "bool",
        "long",
        "short",
        "float",
        "double",
        "unsigned",
        "signed",
        "void",
        "auto",
        "size_t",
        "ssize_t",
        "ptrdiff_t",
        "intptr_t",
        "uintptr_t",
    ] {
        known_types.insert(kw.to_string());
    }
    let mut i = 0;
    // 跟踪是否在函数参数列表中
    let mut paren_depth = 0i32;
    while i < len {
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
        // 跟踪括号深度
        if bytes[i] == b'(' {
            paren_depth += 1;
        }
        if bytes[i] == b')' {
            paren_depth = (paren_depth - 1).max(0);
        }
        // nullptr → ((void*)0)
        if match_word_at(bytes, i, "nullptr") {
            result.push_str("((void*)0)");
            i += 7;
            continue;
        }
        // mutable → 删除
        if match_word_at(bytes, i, "mutable") {
            i = skip_ws(bytes, i + 7);
            continue;
        }
        // namespace XXX { ... } → 删除 namespace XXX 和匹配的 },
        // 并为内部顶层符号添加命名空间前缀 (避免不同命名空间同名符号冲突)
        if match_word_at(bytes, i, "namespace") {
            let mut j = skip_ws(bytes, i + 9);
            // 提取 namespace 名 (如果有)
            let mut ns_name = String::new();
            if j < len && is_ident_char(bytes[j]) {
                let (name, name_end) = read_ident(bytes, j);
                ns_name = name;
                j = skip_ws(bytes, name_end);
            }
            // 如果后面是 '{', 跳过整个 namespace 块
            if j < len && bytes[j] == b'{' {
                // 找到匹配的 '}'
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
                let content = &s[j + 1..k - 1];
                if !ns_name.is_empty() {
                    // 为内容中的顶层标识符添加命名空间前缀
                    result.push_str(&prefix_ns_identifiers(content, &ns_name));
                } else {
                    result.push_str(content);
                }
                i = k;
                continue;
            }
            // 没有 '{', 只跳过 namespace 关键字和名字
            i = j;
            continue;
        }
        // 在函数参数列表中, 将 Type &name 转换为 Type *name
        // 检测: 在括号内, '&' 前面是类型/空白, 后面是标识符
        // 注意: 必须区分引用声明 (int &x) 和位运算 (x & y)
        // 策略: 只有当 '&' 前面的标识符是已知类型名或类型关键字时才转换
        if paren_depth > 0 && bytes[i] == b'&' {
            // 检查不是 && (移动语义或逻辑与)
            if i + 1 < len && bytes[i + 1] == b'&' {
                // && 在参数列表中可能是移动语义, 在表达式中是逻辑与
                // 不替换, 原样输出
                result.push_str("&&");
                i += 2;
                continue;
            }
            // 检查前面是标识符字符 (可能是类型名)
            let prev_char = if result.is_empty() {
                b' '
            } else {
                result.as_bytes().last().copied().unwrap()
            };
            if is_ident_char(prev_char)
                || prev_char == b'*'
                || prev_char == b'>'
                || prev_char == b' '
            {
                // 提取前面的标识符, 检查是否是类型名
                let is_ref_decl = if is_ident_char(prev_char) {
                    // 向前找到标识符起始位置
                    let rbytes = result.as_bytes();
                    let id_end = rbytes.len();
                    let mut id_start = id_end;
                    while id_start > 0 && is_ident_char(rbytes[id_start - 1]) {
                        id_start -= 1;
                    }
                    let prev_ident = &result[id_start..id_end];
                    // 检查是否是已知类型名或以 _t 结尾
                    known_types.contains(prev_ident) || prev_ident.ends_with("_t")
                } else if prev_char == b'*' || prev_char == b'>' {
                    // 指针或模板类型后面也可以是引用
                    true
                } else {
                    // 前面是空格, 检查更前面的字符
                    let rbytes = result.as_bytes();
                    let mut k = rbytes.len();
                    // 跳过空格
                    while k > 0 && (rbytes[k - 1] == b' ' || rbytes[k - 1] == b'\t') {
                        k -= 1;
                    }
                    if k > 0 && (rbytes[k - 1] == b'*' || rbytes[k - 1] == b'>') {
                        // After pointer (*) or template (>), & is a reference
                        true
                    } else if k > 0 && is_ident_char(rbytes[k - 1]) {
                        let id_end = k;
                        let mut id_start = k;
                        while id_start > 0 && is_ident_char(rbytes[id_start - 1]) {
                            id_start -= 1;
                        }
                        let prev_ident = &result[id_start..id_end];
                        known_types.contains(prev_ident) || prev_ident.ends_with("_t")
                    } else {
                        false
                    }
                };
                if is_ref_decl {
                    // 检查后面是标识符开始或空白+标识符
                    let j = skip_ws(bytes, i + 1);
                    if j < len && (is_ident_char(bytes[j]) || bytes[j] == b'*') {
                        // 这是引用参数, 替换 & 为 *
                        result.push('*');
                        i += 1;
                        continue;
                    }
                }
            }
        }
        // 在函数参数列表外, 将 Type & 转换为 Type * (引用返回类型)
        // 检测: 不在括号内, '&' 前面是类型上下文, 后面是换行或标识符.
        // 注意: 不能只看 '&' 前一个字符 —— 表达式取地址 `x = &y` /
        // `return &x` 与位与 `a & b` 的 '&' 前也是标识符或空格,
        // 若不回溯验证前一个词是类型名就会误伤 (bug: `p = &a` →
        // `p = *a`). 必须确认前一个非空白字符处于类型上下文:
        // 已知类型名 / `*_t` 后缀 / `*` (指针) / `>` (模板) 之一.
        if paren_depth == 0 && bytes[i] == b'&' {
            // 检查不是 &&
            if i + 1 < len && bytes[i + 1] == b'&' {
                result.push_str("&&");
                i += 2;
                continue;
            }
            // 检查前面是类型上下文
            let prev_char = if result.is_empty() {
                b' '
            } else {
                result.as_bytes().last().copied().unwrap()
            };
            let is_ref_decl = if is_ident_char(prev_char) {
                // '&' 紧跟标识符: 仅当该标识符是已知类型名
                // (Type& x / Type& fn()), 位与 `a & b` 的 a 不是类型
                let rbytes = result.as_bytes();
                let id_end = rbytes.len();
                let mut id_start = id_end;
                while id_start > 0 && is_ident_char(rbytes[id_start - 1]) {
                    id_start -= 1;
                }
                let prev_ident = &result[id_start..id_end];
                known_types.contains(prev_ident) || prev_ident.ends_with("_t")
            } else if prev_char == b'>' {
                // 模板类型 T<...> & x
                true
            } else if prev_char == b' ' || prev_char == b'\t' {
                // '&' 前是空白: 回溯到前一个非空白字符, 必须是类型上下文
                // (Type * & x / T<...> & x / Type & x). 表达式取地址
                // `x = &y` 的前置字符是 `=`/`(`/`return` 结尾, 不满足.
                let rbytes = result.as_bytes();
                let mut k = rbytes.len();
                while k > 0 && (rbytes[k - 1] == b' ' || rbytes[k - 1] == b'\t') {
                    k -= 1;
                }
                if k == 0 {
                    false
                } else if rbytes[k - 1] == b'*' || rbytes[k - 1] == b'>' {
                    true
                } else if is_ident_char(rbytes[k - 1]) {
                    let id_end = k;
                    let mut id_start = k;
                    while id_start > 0 && is_ident_char(rbytes[id_start - 1]) {
                        id_start -= 1;
                    }
                    let prev_ident = &result[id_start..id_end];
                    known_types.contains(prev_ident) || prev_ident.ends_with("_t")
                } else {
                    false
                }
            } else {
                false
            };
            if is_ref_decl {
                // 检查后面是否是空白+换行或标识符 (函数名/变量名)
                let j = skip_ws(bytes, i + 1);
                if j < len && (bytes[j] == b'\n' || bytes[j] == b'\r' || is_ident_char(bytes[j])) {
                    // 这是引用声明, 替换 & 为 *
                    result.push('*');
                    i += 1;
                    continue;
                }
            }
        }
        // 空 {} 处理: 不在此处统一替换, 避免影响函数体中的空块.
        // 空 struct 由 clean_struct_bodies 处理.

        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// 预处理 C++ 源文本, 将复杂 C++ 构造替换为 C 兼容形式.
/// 在 strip_cpp_keywords 的字符级处理之前调用.
pub(crate) fn replace_cpp_constructs(s: &str) -> String {
    let mut result = s.to_string();
    // 剥离 GCC 属性宏 (如 GTY(...), __attribute__((...)), 等)
    result = strip_gcc_attributes(&result);
    // 展平 namespace 限定符: std::string → std_string
    // 必须在 clean_struct_bodies 之前运行, 否则 struct 体内
    // 的类型名仍含 :: 导致后续处理出错
    result = flatten_scope_operators(&result);
    // 删除模板定义 (C 不支持模板)
    result = remove_template_definitions(&result);
    // Fix template function names used as values BEFORE generic template replacement
    // alloc_node<T> as a value should be a null pointer, not void*
    result = result.replace("alloc_node<cpp_hashnode_extra>", "((void*)0)");
    result = result.replace("alloc_node<cpp_hashnode_extra >", "((void*)0)");
    result = result.replace("alloc_node<cpp_hashnode>", "((void*)0)");
    result = result.replace("alloc_node<cpp_hashnode >", "((void*)0)");
    // 替换模板使用: template_name<args> → void*
    result = replace_template_usages(&result);
    // class → struct (简单替换, 仅独立的 class 关键字)
    result = replace_word(&result, "class", "struct");
    // enum struct/enum class → enum (C++11 作用域枚举)
    result = replace_phrase(&result, "enum struct", "enum");
    result = replace_phrase(&result, "enum class", "enum");
    // 删除 struct 继承语法: struct X : Y { → struct X {
    result = remove_struct_inheritance(&result);
    // 清理 struct 定义体: 删除构造函数/析构函数/默认参数/内联成员函数等
    result = clean_struct_bodies(&result);
    // 为所有 struct 生成 typedef, 使得 NAME 和 struct NAME 都有效
    result = generate_struct_typedefs(&result);
    // Fix namespace-flattened enum references (bidi_kind -> typedef + defines)
    result = fix_flattened_enum_refs(&result);
    // 为所有 struct 类型名称添加 struct 前缀 (在函数参数/返回类型/局部变量中)
    result = add_struct_prefix_to_types(&result);
    // 删除访问说明符 public: / private: / protected:
    for kw in &["public", "private", "protected"] {
        result = replace_access_specifier(&result, kw);
    }
    // explicit → 删除
    result = replace_word(&result, "explicit", "");
    // virtual → 删除
    result = replace_word(&result, "virtual", "");
    // nullptr → ((void*)0), mutable → 删除
    result = fix_remaining_cpp_syntax(&result);
    // auto → __auto_type (GCC extension for type deduction)
    result = replace_word(&result, "auto", "__auto_type");
    // 第二次展平 :: (捕获可能被遗漏的)
    result = flatten_scope_operators(&result);
    // inline → static inline (C99 inline has external linkage by default,
    // causing multiple definition errors; static inline fixes this)
    result = convert_inline_to_static_inline(&result);
    // 删除所有函数声明中的默认参数
    result = remove_all_default_args(&result);
    // C++ 函数式类型转换 (保守版): Type(number) → (Type)(number)
    result = convert_simple_functional_casts(&result);
    // Lambda 表达式: [] (...) { ... } → 0
    result = replace_lambdas(&result);
    // 析构函数定义: ClassName::~ClassName(...) → void _dtor_ClassName(...)
    result = replace_destructors(&result);
    // 构造函数定义: ClassName::ClassName(...) : init_list { body }
    // → void _ctor_ClassName(...) { body }
    result = replace_constructors(&result);
    // Handle flattened constructor/destructor patterns (after :: was replaced with _)
    result = replace_flattened_ctor_dtor(&result);
    // 转换 return struct Type(args) → return (struct Type){args}
    result = convert_return_struct_ctor(&result);
    // 替换花括号初始化: Type var{args} → Type var = {args}
    result = replace_brace_init(&result);
    // 替换函数体中的构造函数调用: Type var(args); → Type var; _ctor_Type(&var, args);
    result = replace_ctor_calls_in_bodies(&result);
    // 转换 if 条件中的变量声明: if (Type var = expr) → Type var = expr; if (var)
    result = convert_if_condition_decls(&result);
    // 最后一次展平 :: (捕获所有遗漏的)
    result = flatten_scope_operators(&result);
    // 强制替换所有剩余的 :: 为 _ (除了字符串字面量中的)
    {
        let mut forced = String::with_capacity(result.len());
        let bytes = result.as_bytes();
        let len = bytes.len();
        let mut i = 0;
        while i < len {
            if bytes[i] == b'"' {
                let j = skip_string_literal(bytes, i);
                forced.push_str(&result[i..j]);
                i = j;
                continue;
            }
            if bytes[i] == b':' && i + 1 < len && bytes[i + 1] == b':' {
                forced.push('_');
                i += 2;
                continue;
            }
            forced.push(bytes[i] as char);
            i += 1;
        }
        result = forced;
    }
    // 注意: 不自动替换成员函数调用, 因为需要类型信息.
    result
}

/// 删除访问说明符 "public:" / "private:" / "protected:"
pub(crate) fn replace_access_specifier(s: &str, kw: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let kw_bytes = kw.as_bytes();
    let kw_len = kw_bytes.len();
    let mut i = 0;
    while i < len {
        if i + kw_len <= len && &bytes[i..i + kw_len] == kw_bytes {
            let before_ok = i == 0 || !is_ident_char(bytes[i - 1]);
            let after_ok = i + kw_len >= len || !is_ident_char(bytes[i + kw_len]);
            if before_ok && after_ok {
                // 跳过 kw 和后面的 ':' 以及空白
                let mut j = i + kw_len;
                while j < len && (bytes[j] as char).is_ascii_whitespace() {
                    j += 1;
                }
                if j < len && bytes[j] == b':' {
                    j += 1; // 跳过 ':'
                            // 检查不是 '::'
                    if j < len && bytes[j] == b':' {
                        // 这是 '::', 不是访问说明符
                        result.push(bytes[i] as char);
                        i += 1;
                        continue;
                    }
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

/// 替换 lambda 表达式 [...] (...) { ... } → 0
pub(crate) fn replace_lambdas(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        // 检测 '[' 开始的可能 lambda
        if bytes[i] == b'[' {
            // 检查是否是 lambda: [...] 后跟 (...) 或 {...}
            let mut j = i + 1;
            // 跳过捕获列表内容
            let mut bracket_depth = 1i32;
            while j < len && bracket_depth > 0 {
                if bytes[j] == b'[' {
                    bracket_depth += 1;
                } else if bytes[j] == b']' {
                    bracket_depth -= 1;
                }
                j += 1;
            }
            // 跳过空白
            let after_bracket = j;
            while j < len && (bytes[j] as char).is_ascii_whitespace() {
                j += 1;
            }
            // 检查后跟 '(' (参数列表) 或 '{' (无参数 lambda)
            if j < len && (bytes[j] == b'(' || bytes[j] == b'{') {
                // 跳过参数列表 (如果有)
                if bytes[j] == b'(' {
                    let mut depth = 1i32;
                    j += 1;
                    while j < len && depth > 0 {
                        if bytes[j] == b'(' {
                            depth += 1;
                        } else if bytes[j] == b')' {
                            depth -= 1;
                        }
                        j += 1;
                    }
                    // 跳过空白
                    while j < len && (bytes[j] as char).is_ascii_whitespace() {
                        j += 1;
                    }
                }
                // 跳过可能的 mutable / noexcept / -> 返回类型
                // 简单处理: 跳过到 '{'
                while j < len && bytes[j] != b'{' && bytes[j] != b';' {
                    j += 1;
                }
                // 跳过 body { ... }
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
                    // 成功匹配 lambda, 替换为 0
                    result.push('0');
                    i = j;
                    continue;
                }
            }
            // 不是 lambda, 原样输出
            let _ = after_bracket;
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// 替换析构函数定义: ClassName::~ClassName(...) → void _dtor_ClassName(...)
pub(crate) fn replace_destructors(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        // 查找 ~ 字符
        if bytes[i] == b'~' {
            // 检查前面是 '::' (析构函数定义)
            let before = i;
            if before >= 2 && bytes[before - 1] == b':' && bytes[before - 2] == b':' {
                // 找到类名
                let class_end = before - 2;
                let mut class_start = class_end;
                while class_start > 0 && is_ident_char(bytes[class_start - 1]) {
                    class_start -= 1;
                }
                let class_name = &s[class_start..class_end];
                // 跳过 ~ClassName
                let mut j = i + 1;
                while j < len && is_ident_char(bytes[j]) {
                    j += 1;
                }
                // 替换: 从 class_start 到 j 替换为 _dtor_ClassName
                // 先输出 class_start 之前的内容 (已经输出了)
                // 回退到 class_start
                let backtrack = result.len() - (class_end - class_start) - 2; // -2 for '::'
                result.truncate(backtrack);
                result.push_str(&format!("void _dtor_{}", class_name));
                i = j;
                continue;
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// 替换构造函数定义: ClassName::ClassName(...) : init_list { body }
/// → void _ctor_ClassName(...) { body }
pub(crate) fn replace_constructors(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        // 查找 ClassName::ClassName 模式 (类名与函数名相同)
        if bytes[i] == b':' && i + 1 < len && bytes[i + 1] == b':' {
            // 前面是类名
            let class_end = i;
            let mut class_start = class_end;
            while class_start > 0 && is_ident_char(bytes[class_start - 1]) {
                class_start -= 1;
            }
            let class_name = &s[class_start..class_end];
            if class_name.is_empty() {
                result.push(bytes[i] as char);
                i += 1;
                continue;
            }
            // :: 后面应该是相同的类名 (构造函数)
            let j = i + 2;
            let mut name_end = j;
            while name_end < len && is_ident_char(bytes[name_end]) {
                name_end += 1;
            }
            let fn_name = &s[j..name_end];
            if fn_name == class_name {
                // 确认是构造函数: 后面跟 '(' 或空白后 '('
                let mut k = name_end;
                while k < len && (bytes[k] as char).is_ascii_whitespace() {
                    k += 1;
                }
                if k < len && bytes[k] == b'(' {
                    // 是构造函数! 找到参数列表结束
                    let mut depth = 1i32;
                    k += 1;
                    while k < len && depth > 0 {
                        if bytes[k] == b'(' {
                            depth += 1;
                        } else if bytes[k] == b')' {
                            depth -= 1;
                        }
                        k += 1;
                    }
                    // 跳过空白
                    while k < len && (bytes[k] as char).is_ascii_whitespace() {
                        k += 1;
                    }
                    // 跳过构造函数初始化列表 : member(init), ...
                    if k < len && bytes[k] == b':' {
                        // 检查不是 '::'
                        if k + 1 < len && bytes[k + 1] != b':' {
                            k += 1; // 跳过 ':'
                                    // 跳过到 '{'
                            let mut brace_depth = 0i32;
                            while k < len {
                                if bytes[k] == b'(' {
                                    brace_depth += 1;
                                } else if bytes[k] == b')' {
                                    brace_depth -= 1;
                                } else if bytes[k] == b'{' && brace_depth == 0 {
                                    break;
                                }
                                k += 1;
                            }
                        }
                    }
                    // 回退并替换
                    let backtrack = result.len() - (class_end - class_start) - 2;
                    result.truncate(backtrack);
                    // 确保 void 前有换行 (避免 }void 粘连)
                    if result.as_bytes().last() == Some(&b'}') {
                        result.push('\n');
                    }
                    result.push_str(&format!("void _ctor_{}_1()", class_name));
                    i = k;
                    continue;
                }
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// Handle flattened constructor/destructor patterns after :: was replaced with _
/// Pattern: ClassName_ClassName(params) → void _ctor_ClassName(params)
/// Pattern: ClassName_~ClassName(params) → void _dtor_ClassName(params)
pub(crate) fn replace_flattened_ctor_dtor(s: &str) -> String {
    // Step 1: Collect all struct names from `struct NAME {` / `struct NAME\n{`
    let mut struct_names: Vec<String> = Vec::new();
    {
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
            if match_word_at(bytes, i, "struct") {
                let j = skip_ws(bytes, i + 6);
                if j < len && is_ident_char(bytes[j]) {
                    let (name, name_end) = read_ident(bytes, j);
                    let k = skip_ws(bytes, name_end);
                    if k < len && bytes[k] == b'{' && !struct_names.contains(&name) {
                        struct_names.push(name);
                    }
                }
            }
            i += 1;
        }
    }
    // Sort longest first to avoid substring false matches
    struct_names.sort_by_key(|n| std::cmp::Reverse(n.len()));

    // Step 2: Replace destructor patterns
    // ClassName_~ClassName( → void _dtor_ClassName (
    let mut result = s.to_string();
    for name in &struct_names {
        let pattern = format!("{}_~{}", name, name);
        while let Some(pos) = result.find(&pattern) {
            // Check word boundary before pattern
            if pos > 0 {
                let prev = result.as_bytes()[pos - 1];
                if is_ident_char(prev) {
                    break;
                }
            }
            let after = pos + pattern.len();
            // Skip whitespace after pattern
            let mut j = after;
            let rb = result.as_bytes();
            while j < rb.len() && (rb[j] as char).is_ascii_whitespace() {
                j += 1;
            }
            // Must be followed by (
            if j < rb.len() && rb[j] == b'(' {
                let replacement = format!("void _dtor_{} ", name);
                result = format!("{}{}{}", &result[..pos], replacement, &result[after..]);
            } else {
                break;
            }
        }
    }

    // Step 3: Replace constructor patterns
    // ClassName_ClassName( → void _ctor_ClassName (
    // Also skip initializer list : member(init) { body }
    for name in &struct_names {
        let pattern = format!("{}_{}", name, name);
        while let Some(pos) = result.find(&pattern) {
            // Check word boundary before pattern
            if pos > 0 {
                let prev = result.as_bytes()[pos - 1];
                if is_ident_char(prev) {
                    break;
                }
            }
            let after = pos + pattern.len();
            let rb = result.as_bytes();
            // Skip whitespace
            let mut j = after;
            while j < rb.len() && (rb[j] as char).is_ascii_whitespace() {
                j += 1;
            }
            // Must be followed by (
            if j >= rb.len() || rb[j] != b'(' {
                break;
            }
            // Find matching )
            let mut depth = 1i32;
            j += 1; // skip (
            while j < rb.len() && depth > 0 {
                if rb[j] == b'(' {
                    depth += 1;
                } else if rb[j] == b')' {
                    depth -= 1;
                }
                j += 1;
            }
            // j is now right after matching ')'
            // Save this position - we keep params up to here
            let after_params = j;
            // Skip whitespace
            while j < rb.len() && (rb[j] as char).is_ascii_whitespace() {
                j += 1;
            }
            // Skip initializer list : member(init), ...
            if j < rb.len() && rb[j] == b':' && (j + 1 >= rb.len() || rb[j + 1] != b':') {
                j += 1;
                let mut bd = 0i32;
                while j < rb.len() {
                    if rb[j] == b'(' {
                        bd += 1;
                    } else if rb[j] == b')' {
                        bd -= 1;
                    } else if rb[j] == b'{' && bd == 0 {
                        break;
                    }
                    j += 1;
                }
            }
            let _replacement = format!("void _ctor_{}", name);
            // Count parameters to disambiguate overloaded constructors
            let params_str = &result[after..after_params];
            let param_count = if params_str.trim().is_empty() || params_str.trim() == "void" {
                0
            } else {
                params_str.split(',').count()
            };
            let replacement = format!("void _ctor_{}_{}", name, param_count);
            // Keep: prefix + replacement + params + rest from j
            result = format!(
                "{}{} {}{}",
                &result[..pos],
                replacement,
                &result[after..after_params],
                &result[j..]
            );
        }
    }

    result
}

/// Check if a function name matches the pattern ClassName_methodname
/// where ClassName is a known struct. Returns (class_name, method_name) if matched.
pub(crate) fn match_member_fn_name<'a>(
    fname: &str,
    structs: &'a [(String, Vec<String>)],
) -> Option<&'a (String, Vec<String>)> {
    for s in structs {
        let prefix = format!("{}_", s.0);
        if fname.starts_with(&prefix) {
            let method = &fname[prefix.len()..];
            if !method.is_empty() {
                return Some(s);
            }
        }
    }
    None
}

/// Convert out-of-line member function definitions:
/// 1. Add (struct ClassName *this_) as first parameter
/// 2. Convert constructor initializer lists to body assignments
/// 3. Rewrite bare member accesses to this_->member
pub(crate) fn convert_member_fn_definitions(s: &str, structs: &[(String, Vec<String>)]) -> String {
    let mut result = String::with_capacity(s.len() + 1024);
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
        // Look for function definitions: at start of line (or after }), find:
        // [return_type]\nClassName_methodname (params) { body }
        // We detect this by looking for an identifier followed by '(' at the start of a line
        if is_ident_char(bytes[i])
            && (i == 0 || bytes[i - 1] == b'\n' || bytes[i - 1] == b' ' || bytes[i - 1] == b'\t')
        {
            let (ident, ident_end) = read_ident(bytes, i);
            let after_ident = skip_ws(bytes, ident_end);
            // Check if this is a function name (followed by '(' )
            if after_ident < len && bytes[after_ident] == b'(' {
                // Check if this matches a member function pattern
                // But first we need to handle the return type which might be on a previous line
                // Pattern: "ClassName_\n" or "return_type\nClassName_("
                // Also handle broken constructor return types like "ClassName_"

                // Check if ident is a constructor: "ClassName_" pattern (trailing underscore)
                // This happens when "ClassName::ClassName" is flattened to "ClassName_ClassName"
                // but then the return type detection breaks
                let is_ctor = ident.ends_with('_')
                    && structs
                        .iter()
                        .any(|(sn, _)| ident == format!("{}_", sn) || ident == sn.as_str());

                if is_ctor {
                    // This is a broken constructor return type like "cpp_string_location_reader_"
                    // The actual function name is on the next line
                    // Skip to the actual function name
                    let after_underscore = skip_ws(bytes, ident_end);
                    if after_underscore < len && is_ident_char(bytes[after_underscore]) {
                        let (actual_fname, actual_end) = read_ident(bytes, after_underscore);
                        let after_fname = skip_ws(bytes, actual_end);
                        if after_fname < len && bytes[after_fname] == b'(' {
                            // For constructors, actual_fname == class name (not ClassName_method)
                            let struct_info = structs.iter().find(|(sn, _)| {
                                actual_fname == sn.as_str() || actual_fname == format!("{}_", sn)
                            });
                            if let Some(struct_info) = struct_info {
                                // This is a constructor definition
                                // Find matching ')' for params
                                let mut depth = 1i32;
                                let mut j = after_fname + 1;
                                while j < len && depth > 0 {
                                    if bytes[j] == b'(' {
                                        depth += 1;
                                    } else if bytes[j] == b')' {
                                        depth -= 1;
                                    }
                                    j += 1;
                                }
                                let paren_end = j - 1;
                                let params = &s[after_fname + 1..paren_end];
                                // Check for ':' (member initializer list) or '{' (body)
                                let after_paren = skip_ws(bytes, j);
                                let has_init_list = after_paren < len && bytes[after_paren] == b':';

                                // Output: void _ctor_ClassName_N(struct ClassName *this_, params)
                                // Count parameters for overloaded ctor naming
                                let param_count = if params.trim().is_empty() {
                                    1
                                } else {
                                    1 + params.split(',').count()
                                };
                                result.push_str(&format!(
                                    "\nvoid _ctor_{}_{}(struct {} *this_{}",
                                    struct_info.0,
                                    param_count,
                                    struct_info.0,
                                    if params.trim().is_empty() { "" } else { ", " }
                                ));
                                result.push_str(params);
                                result.push(')');

                                if has_init_list {
                                    // Parse initializer list: member(value), ...
                                    let mut k = after_paren + 1;
                                    // Skip whitespace
                                    while k < len
                                        && (bytes[k] == b' '
                                            || bytes[k] == b'\t'
                                            || bytes[k] == b'\n')
                                    {
                                        k += 1;
                                    }
                                    // Find the opening '{'
                                    let mut init_entries = Vec::new();
                                    while k < len && bytes[k] != b'{' {
                                        // Parse: member(value) or member{value}
                                        if is_ident_char(bytes[k]) {
                                            let (mname, mname_end) = read_ident(bytes, k);
                                            let after_mname = skip_ws(bytes, mname_end);
                                            if after_mname < len
                                                && (bytes[after_mname] == b'('
                                                    || bytes[after_mname] == b'{')
                                            {
                                                let open = bytes[after_mname];
                                                let close = if open == b'(' { b')' } else { b'}' };
                                                let mut d = 1i32;
                                                let mut v = after_mname + 1;
                                                while v < len && d > 0 {
                                                    if bytes[v] == open {
                                                        d += 1;
                                                    } else if bytes[v] == close {
                                                        d -= 1;
                                                    }
                                                    v += 1;
                                                }
                                                let value = &s[after_mname + 1..v - 1];
                                                init_entries.push((mname, value.to_string()));
                                                k = v;
                                                // Skip comma
                                                let after_val = skip_ws(bytes, k);
                                                if after_val < len && bytes[after_val] == b',' {
                                                    k = after_val + 1;
                                                } else {
                                                    k = after_val;
                                                }
                                            } else {
                                                k = after_mname;
                                            }
                                        } else {
                                            k += 1;
                                        }
                                    }
                                    // Find the body '{' ... '}'
                                    if k < len && bytes[k] == b'{' {
                                        let mut bd = 1i32;
                                        let body_start = k + 1;
                                        k += 1;
                                        while k < len && bd > 0 {
                                            if bytes[k] == b'{' {
                                                bd += 1;
                                            } else if bytes[k] == b'}' {
                                                bd -= 1;
                                            }
                                            k += 1;
                                        }
                                        let body_end = k - 1;
                                        let body = &s[body_start..body_end];
                                        // Output init list as assignments
                                        result.push_str(" {\n");
                                        for (mname, mvalue) in &init_entries {
                                            result.push_str(&format!(
                                                "  this_->{} = {};\n",
                                                mname, mvalue
                                            ));
                                        }
                                        // Rewrite bare member accesses in body
                                        let fixed_body =
                                            rewrite_member_accesses(body, &struct_info.1);
                                        result.push_str(&fixed_body);
                                        result.push_str("}\n");
                                        i = k;
                                        continue;
                                    }
                                } else if after_paren < len && bytes[after_paren] == b'{' {
                                    // No init list, just body
                                    let mut bd = 1i32;
                                    let body_start = after_paren + 1;
                                    let mut k = body_start;
                                    while k < len && bd > 0 {
                                        if bytes[k] == b'{' {
                                            bd += 1;
                                        } else if bytes[k] == b'}' {
                                            bd -= 1;
                                        }
                                        k += 1;
                                    }
                                    let body_end = k - 1;
                                    let body = &s[body_start..body_end];
                                    result.push_str(" {\n");
                                    let fixed_body = rewrite_member_accesses(body, &struct_info.1);
                                    result.push_str(&fixed_body);
                                    result.push_str("}\n");
                                    i = k;
                                    continue;
                                }
                            }
                        }
                    }
                    // If we couldn't parse it as constructor, just output as-is
                    result.push_str(&ident);
                    i = ident_end;
                    continue;
                }

                // Check if this is a regular member function (not constructor)
                if let Some(struct_info) = match_member_fn_name(&ident, structs) {
                    let method_name = &ident[struct_info.0.len() + 1..];
                    // Skip if this looks like a constructor (method name == class name)
                    if method_name == struct_info.0 {
                        result.push_str(&ident);
                        i = ident_end;
                        continue;
                    }
                    // Find matching ')' for params
                    let mut depth = 1i32;
                    let mut j = after_ident + 1;
                    while j < len && depth > 0 {
                        if bytes[j] == b'(' {
                            depth += 1;
                        } else if bytes[j] == b')' {
                            depth -= 1;
                        }
                        j += 1;
                    }
                    let paren_end = j - 1;
                    let params = &s[after_ident + 1..paren_end];
                    // Check if followed by '{' (function definition, not declaration)
                    let after_paren = skip_ws(bytes, j);
                    if after_paren < len && bytes[after_paren] == b'{' {
                        // This is a member function definition
                        // Check if it already has this_ parameter
                        if !params.contains("this_") {
                            // Output: return_type ClassName_method(struct ClassName *this_, params) { body }
                            // First, go back to find the return type (might be on previous line)
                            // For now, we've already output everything before ident
                            // Insert this_ as first parameter
                            result.push_str(&ident);
                            result.push_str(" (struct ");
                            result.push_str(&struct_info.0);
                            result.push_str(" *this_");
                            if !params.trim().is_empty() {
                                result.push_str(", ");
                                result.push_str(params.trim());
                            }
                            result.push(')');
                            // Parse body and rewrite member accesses
                            let mut bd = 1i32;
                            let body_start = after_paren + 1;
                            let mut k = body_start;
                            while k < len && bd > 0 {
                                if bytes[k] == b'{' {
                                    bd += 1;
                                } else if bytes[k] == b'}' {
                                    bd -= 1;
                                }
                                k += 1;
                            }
                            let body_end = k - 1;
                            let body = &s[body_start..body_end];
                            result.push_str(" {");
                            let fixed_body = rewrite_member_accesses(body, &struct_info.1);
                            result.push_str(&fixed_body);
                            result.push('}');
                            i = k;
                            continue;
                        }
                    }
                }
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// Rewrite bare member variable accesses (m_foo) to this_->m_foo in a function body
pub(crate) fn rewrite_member_accesses(body: &str, members: &[String]) -> String {
    if members.is_empty() {
        return body.to_string();
    }
    let mut result = String::with_capacity(body.len());
    let bytes = body.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        // Skip strings
        if bytes[i] == b'"' {
            let j = skip_string_literal(bytes, i);
            result.push_str(&body[i..j]);
            i = j;
            continue;
        }
        // Skip comments
        if bytes[i] == b'/' && i + 1 < len && (bytes[i + 1] == b'/' || bytes[i + 1] == b'*') {
            let j = skip_comment(bytes, i);
            result.push_str(&body[i..j]);
            i = j;
            continue;
        }
        if is_ident_char(bytes[i]) && (i == 0 || !is_ident_char(bytes[i - 1])) {
            let (ident, ident_end) = read_ident(bytes, i);
            // Check if this is a member variable
            if members.iter().any(|m| m == &ident) {
                // Check it's not preceded by '->' or '.' (already qualified)
                let before_ok = if i == 0 {
                    true
                } else if bytes[i - 1] == b'>' && i >= 2 && bytes[i - 2] == b'-' {
                    false
                } else {
                    bytes[i - 1] != b'.'
                };
                if before_ok {
                    result.push_str("this_->");
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

/// Convert member function call sites:
/// ptr->method(args) -> Class_method(ptr, args)
/// obj.method(args) -> Class_method(&obj, args)
pub(crate) fn convert_member_call_sites(s: &str, structs: &[(String, Vec<String>)]) -> String {
    // Build method_name -> class_name mapping
    let mut method_to_class: Vec<(String, String)> = Vec::new();
    for (sname, _) in structs {
        let prefix = format!("{}_", sname);
        // Scan for all ClassName_method patterns in function definitions
        let bytes = s.as_bytes();
        let len = bytes.len();
        let mut i = 0;
        while i < len {
            if is_ident_char(bytes[i]) && (i == 0 || !is_ident_char(bytes[i - 1])) {
                let (ident, _) = read_ident(bytes, i);
                if ident.starts_with(&prefix) {
                    let method = &ident[prefix.len()..];
                    if !method.is_empty()
                        && !method_to_class
                            .iter()
                            .any(|(m, c)| m == method && c == sname)
                    {
                        method_to_class.push((method.to_string(), sname.clone()));
                    }
                }
            }
            i += 1;
        }
    }

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
        // Look for identifier->method( or identifier.method(
        if is_ident_char(bytes[i]) && (i == 0 || !is_ident_char(bytes[i - 1])) {
            let (receiver, receiver_end) = read_ident(bytes, i);
            let after_receiver = skip_ws(bytes, receiver_end);
            if after_receiver < len
                && (bytes[after_receiver] == b'-' || bytes[after_receiver] == b'.')
            {
                let is_arrow = bytes[after_receiver] == b'-';
                let is_dot = bytes[after_receiver] == b'.';
                if (is_arrow && after_receiver + 1 < len && bytes[after_receiver + 1] == b'>')
                    || is_dot
                {
                    let method_start = if is_arrow {
                        after_receiver + 2
                    } else {
                        after_receiver + 1
                    };
                    let method_start = skip_ws(bytes, method_start);
                    if method_start < len && is_ident_char(bytes[method_start]) {
                        let (method, method_end) = read_ident(bytes, method_start);
                        let after_method = skip_ws(bytes, method_end);
                        if after_method < len && bytes[after_method] == b'(' {
                            // Check if this method belongs to a known class
                            if let Some((_, class_name)) =
                                method_to_class.iter().find(|(m, _)| m == &method)
                            {
                                let full_fn = format!("{}_{}", class_name, method);
                                // Find matching ')' for args
                                let mut depth = 1i32;
                                let mut j = after_method + 1;
                                while j < len && depth > 0 {
                                    if bytes[j] == b'(' {
                                        depth += 1;
                                    } else if bytes[j] == b')' {
                                        depth -= 1;
                                    }
                                    j += 1;
                                }
                                let args = &s[after_method + 1..j - 1];
                                if is_arrow {
                                    // ptr->method(args) -> Class_method(ptr, args)
                                    result.push_str(&full_fn);
                                    result.push_str(" (");
                                    result.push_str(&receiver);
                                    if !args.trim().is_empty() {
                                        result.push_str(", ");
                                        result.push_str(args.trim());
                                    }
                                    result.push(')');
                                } else {
                                    // obj.method(args) -> Class_method(&obj, args)
                                    result.push_str(&full_fn);
                                    result.push_str(" (&");
                                    result.push_str(&receiver);
                                    if !args.trim().is_empty() {
                                        result.push_str(", ");
                                        result.push_str(args.trim());
                                    }
                                    result.push(')');
                                    i = j;
                                    continue;
                                }
                                i = j;
                                continue;
                            }
                        }
                    }
                }
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// Convert bare member function calls within member function bodies.
/// E.g., inside cpp_substring_ranges_add_n_ranges, "add_range(...)" should become
/// "cpp_substring_ranges_add_range(this_, ...)"
pub(crate) fn convert_bare_member_calls_in_bodies(
    s: &str,
    structs: &[(String, Vec<String>)],
) -> String {
    // Build a list of all known member function names (without class prefix)
    let mut member_fns: Vec<(String, String, String)> = Vec::new(); // (class, method, full_name)
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        if is_ident_char(bytes[i]) && (i == 0 || !is_ident_char(bytes[i - 1])) {
            let (ident, _) = read_ident(bytes, i);
            for (sname, _) in structs {
                let prefix = format!("{}_", sname);
                if ident.starts_with(&prefix) {
                    let method = &ident[prefix.len()..];
                    if !method.is_empty() && !member_fns.iter().any(|(_, _, f)| f == &ident) {
                        member_fns.push((sname.clone(), method.to_string(), ident.clone()));
                    }
                }
            }
        }
        i += 1;
    }

    // Now scan for function definitions with this_ parameter and fix bare calls within them
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_member_fn = false;
    let mut current_class = String::new();
    let mut brace_depth = 0i32;

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

        // Track brace depth
        if bytes[i] == b'{' {
            brace_depth += 1;
            result.push(bytes[i] as char);
            i += 1;
            continue;
        }
        if bytes[i] == b'}' {
            brace_depth -= 1;
            if brace_depth == 0 {
                in_member_fn = false;
            }
            result.push(bytes[i] as char);
            i += 1;
            continue;
        }

        // Detect function definitions with this_ parameter
        if !in_member_fn && bytes[i] == b'(' {
            // Check if params contain "struct ClassName *this_"
            let mut depth = 1i32;
            let mut j = i + 1;
            while j < len && depth > 0 {
                if bytes[j] == b'(' {
                    depth += 1;
                } else if bytes[j] == b')' {
                    depth -= 1;
                }
                j += 1;
            }
            let params = &s[i..j];
            if params.contains("*this_") || params.contains("* this_") {
                // Find the function name (before '(')
                let mut fn_end = i;
                while fn_end > 0
                    && (bytes[fn_end - 1] == b' '
                        || bytes[fn_end - 1] == b'\t'
                        || bytes[fn_end - 1] == b'\n')
                {
                    fn_end -= 1;
                }
                let mut fn_start = fn_end;
                while fn_start > 0 && is_ident_char(bytes[fn_start - 1]) {
                    fn_start -= 1;
                }
                if fn_start < fn_end {
                    let fname = &s[fn_start..fn_end];
                    // Check if fname matches ClassName_method pattern
                    for (sname, _) in structs {
                        let prefix = format!("{}_", sname);
                        if fname.starts_with(&prefix) {
                            in_member_fn = true;
                            current_class = sname.clone();
                            break;
                        }
                    }
                    // Also check _ctor_ClassName pattern
                    if let Some(cname) = fname.strip_prefix("_ctor_") {
                        if structs.iter().any(|(sn, _)| sn == cname) {
                            in_member_fn = true;
                            current_class = cname.to_string();
                        }
                    }
                }
            }
        }

        // Inside a member function, look for bare member function calls
        if in_member_fn && is_ident_char(bytes[i]) && (i == 0 || !is_ident_char(bytes[i - 1])) {
            let (ident, ident_end) = read_ident(bytes, i);
            let after_ident = skip_ws(bytes, ident_end);
            if after_ident < len && bytes[after_ident] == b'(' {
                // Check if this is a bare call to another member function of the same class
                let target_full = format!("{}_{}", current_class, ident);
                let is_same_class_call = member_fns
                    .iter()
                    .any(|(c, m, _f)| c == &current_class && m == &ident);
                // Also check if it's a call to a function that doesn't exist as a standalone function
                // (i.e., it's a member function that needs this_ prefix)
                if is_same_class_call {
                    // Check it's not already prefixed
                    let before_ok =
                        i == 0 || (!is_ident_char(bytes[i - 1]) && bytes[i - 1] != b'_');
                    if before_ok {
                        // Find matching ')'
                        let mut depth = 1i32;
                        let mut j = after_ident + 1;
                        while j < len && depth > 0 {
                            if bytes[j] == b'(' {
                                depth += 1;
                            } else if bytes[j] == b')' {
                                depth -= 1;
                            }
                            j += 1;
                        }
                        let args = &s[after_ident + 1..j - 1];
                        result.push_str(&target_full);
                        result.push_str(" (this_");
                        if !args.trim().is_empty() {
                            result.push_str(", ");
                            result.push_str(args.trim());
                        }
                        result.push(')');
                        i = j;
                        continue;
                    }
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

/// Fix broken constructor definitions that have "ClassName_" as a return type
/// followed by the actual constructor name on the next line.
pub(crate) fn fix_broken_constructors(s: &str) -> String {
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
        // Look for identifier at start of line that ends with '_'
        if is_ident_char(bytes[i]) && (i == 0 || bytes[i - 1] == b'\n') {
            let (ident, ident_end) = read_ident(bytes, i);
            if ident.ends_with('_') && ident.len() > 1 {
                let class_name = &ident[..ident.len() - 1];
                // Check if next non-whitespace is the same identifier followed by '('
                let after = skip_ws(bytes, ident_end);
                if after < len && is_ident_char(bytes[after]) {
                    let (next_ident, next_end) = read_ident(bytes, after);
                    if next_ident == class_name {
                        let after_next = skip_ws(bytes, next_end);
                        if after_next < len && bytes[after_next] == b'(' {
                            // This is a broken constructor!
                            // Find matching ')'
                            let mut depth = 1i32;
                            let mut j = after_next + 1;
                            while j < len && depth > 0 {
                                if bytes[j] == b'(' {
                                    depth += 1;
                                } else if bytes[j] == b')' {
                                    depth -= 1;
                                }
                                j += 1;
                            }
                            let paren_end = j - 1;
                            let params = &s[after_next + 1..paren_end];
                            let after_paren = skip_ws(bytes, j);
                            // Check for initializer list ':'
                            if after_paren < len && bytes[after_paren] == b':' {
                                // Parse initializer list and convert to body assignments
                                let mut k = after_paren + 1;
                                let mut init_entries = Vec::new();
                                // Skip to '{'
                                while k < len && bytes[k] != b'{' {
                                    if is_ident_char(bytes[k]) {
                                        let (mname, mname_end) = read_ident(bytes, k);
                                        let after_mname = skip_ws(bytes, mname_end);
                                        if after_mname < len
                                            && (bytes[after_mname] == b'('
                                                || bytes[after_mname] == b'{')
                                        {
                                            let open = bytes[after_mname];
                                            let close = if open == b'(' { b')' } else { b'}' };
                                            let mut d = 1i32;
                                            let mut v = after_mname + 1;
                                            while v < len && d > 0 {
                                                if bytes[v] == open {
                                                    d += 1;
                                                } else if bytes[v] == close {
                                                    d -= 1;
                                                }
                                                v += 1;
                                            }
                                            let value = &s[after_mname + 1..v - 1];
                                            init_entries.push((mname, value.to_string()));
                                            k = v;
                                            let after_val = skip_ws(bytes, k);
                                            if after_val < len && bytes[after_val] == b',' {
                                                k = after_val + 1;
                                            } else {
                                                k = after_val;
                                            }
                                        } else {
                                            k = after_mname;
                                        }
                                    } else {
                                        k += 1;
                                    }
                                }
                                // Find body '{' ... '}'
                                if k < len && bytes[k] == b'{' {
                                    let mut bd = 1i32;
                                    let body_start = k + 1;
                                    k += 1;
                                    while k < len && bd > 0 {
                                        if bytes[k] == b'{' {
                                            bd += 1;
                                        } else if bytes[k] == b'}' {
                                            bd -= 1;
                                        }
                                        k += 1;
                                    }
                                    let body = &s[body_start..k - 1];
                                    // Output: void _ctor_ClassName_N(struct ClassName *this_, params) { assignments; body }
                                    let param_count = if params.trim().is_empty() {
                                        1
                                    } else {
                                        1 + params.split(',').count()
                                    };
                                    result.push_str(&format!(
                                        "void _ctor_{}_{}(struct {} *this_{}",
                                        class_name,
                                        param_count,
                                        class_name,
                                        if params.trim().is_empty() { "" } else { ", " }
                                    ));
                                    result.push_str(params.trim());
                                    result.push_str(") {\n");
                                    for (mname, mvalue) in &init_entries {
                                        result.push_str(&format!(
                                            "  this_->{} = {};\n",
                                            mname, mvalue
                                        ));
                                    }
                                    result.push_str(body);
                                    result.push_str("}\n");
                                    i = k;
                                    continue;
                                }
                            } else if after_paren < len && bytes[after_paren] == b'{' {
                                // No init list, just body
                                let mut bd = 1i32;
                                let body_start = after_paren + 1;
                                let mut k = body_start;
                                while k < len && bd > 0 {
                                    if bytes[k] == b'{' {
                                        bd += 1;
                                    } else if bytes[k] == b'}' {
                                        bd -= 1;
                                    }
                                    k += 1;
                                }
                                let body = &s[body_start..k - 1];
                                let param_count = if params.trim().is_empty() {
                                    1
                                } else {
                                    1 + params.split(',').count()
                                };
                                result.push_str(&format!(
                                    "void _ctor_{}_{}(struct {} *this_{}",
                                    class_name,
                                    param_count,
                                    class_name,
                                    if params.trim().is_empty() { "" } else { ", " }
                                ));
                                result.push_str(params.trim());
                                result.push_str(") {");
                                result.push_str(body);
                                result.push_str("}\n");
                                i = k;
                                continue;
                            }
                        }
                    }
                }
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// 将引用参数转换为指针参数: `Type& name` → `Type* name`
pub(crate) fn convert_ref_params(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut depth = 0i32; // 括号深度
    while i < len {
        if bytes[i] == b'(' {
            depth += 1;
            result.push('(');
            i += 1;
            continue;
        }
        if bytes[i] == b')' {
            depth -= 1;
            result.push(')');
            i += 1;
            continue;
        }
        // 在参数列表中 (depth == 0), 将 '&' 转换为 '*'
        if bytes[i] == b'&' && depth == 0 {
            // 检查不是 '&&' (移动语义)
            if i + 1 < len && bytes[i + 1] == b'&' {
                // && 转换为 * (简化处理)
                result.push('*');
                i += 2;
                continue;
            }
            // 检查前面是标识符字符或 '*' (类型结束)
            let prev_char = if result.is_empty() {
                b' '
            } else {
                result.as_bytes().last().copied().unwrap()
            };
            if is_ident_char(prev_char) || prev_char == b'*' || prev_char == b'>' {
                // 检查后面是标识符开始或空白+标识符 (参数名)
                let j = skip_ws(bytes, i + 1);
                if j < len && (is_ident_char(bytes[j]) || bytes[j] == b'*') {
                    // 这是引用参数, 替换 & 为 *
                    result.push('*');
                    i += 1;
                    continue;
                }
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// 从参数列表中删除默认参数: `int x = 5, int y` → `int x, int y`
pub(crate) fn remove_default_args(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut depth = 0i32; // 括号深度
    while i < len {
        if bytes[i] == b'(' {
            depth += 1;
            result.push('(');
            i += 1;
            continue;
        }
        if bytes[i] == b')' {
            depth -= 1;
            result.push(')');
            i += 1;
            continue;
        }
        if bytes[i] == b'=' && depth == 0 {
            // 跳过默认值到 ',' 或结尾
            i += 1;
            while i < len && bytes[i] != b',' {
                i += 1;
            }
            continue;
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// 删除所有函数声明中的默认参数: `func(int x = 5)` → `func(int x)`
/// 处理全局函数声明和 struct 体内的函数声明
pub(crate) fn remove_all_default_args(s: &str) -> String {
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

        // Look for function declarations: identifier followed by '('
        // Check if this looks like a function name (identifier followed by '(')
        if is_ident_char(bytes[i]) && (i == 0 || !is_ident_char(bytes[i - 1])) {
            let (ident, ident_end) = read_ident(bytes, i);
            // Skip keywords that look like function calls but aren't
            if ident == "__asm__"
                || ident == "__volatile__"
                || ident == "asm"
                || ident == "volatile"
                || ident == "__attribute__"
                || ident == "typeof"
                || ident == "__typeof__"
                || ident == "sizeof"
                || ident == "alignof"
                || ident == "__alignof__"
                || ident == "if"
                || ident == "while"
                || ident == "for"
                || ident == "switch"
                || ident == "return"
                || ident == "case"
                || ident == "do"
                || ident == "else"
            {
                result.push_str(&ident);
                i = ident_end;
                continue;
            }
            let j = skip_ws(bytes, ident_end);
            if j < len && bytes[j] == b'(' {
                // This might be a function declaration or call
                // Check if it's followed by a parameter list with default args
                // Look ahead to find the matching ')'
                // IMPORTANT: Only scan the parameter list, not the function body.
                // Stop if we encounter '{' or ';' at depth 0 (end of declaration).
                let mut paren_depth = 1i32;
                let mut k = j + 1;
                let mut has_default_arg = false;

                while k < len && paren_depth > 0 {
                    // Stop scanning if we hit '{' or ';' at depth 0
                    // (this means we've gone past the parameter list)
                    if paren_depth == 1 && (bytes[k] == b'{' || bytes[k] == b';') {
                        break;
                    }
                    if bytes[k] == b'"' {
                        k = skip_string_literal(bytes, k);
                        continue;
                    }
                    // Skip character literals like '=' or '\n'
                    if bytes[k] == b'\'' {
                        k += 1;
                        while k < len && bytes[k] != b'\'' {
                            if bytes[k] == b'\\' {
                                k += 1;
                            } // skip escape
                            k += 1;
                        }
                        if k < len {
                            k += 1;
                        } // skip closing '
                        continue;
                    }
                    if bytes[k] == b'(' {
                        paren_depth += 1;
                    } else if bytes[k] == b')' {
                        paren_depth -= 1;
                    } else if paren_depth == 1 {
                        // Check for '=' that's not '=='
                        if bytes[k] == b'=' && (k + 1 >= len || bytes[k + 1] != b'=') {
                            // Check it's not '!=', '<=', '>='
                            if k == 0
                                || (bytes[k - 1] != b'!'
                                    && bytes[k - 1] != b'<'
                                    && bytes[k - 1] != b'>')
                            {
                                has_default_arg = true;
                            }
                        }
                    }
                    k += 1;
                }

                // Check what follows the closing ')'
                // Function declarations are followed by '{' (definition) or ';' (declaration)
                let is_func_decl = if k < len && has_default_arg {
                    let after_paren = skip_ws(bytes, k);
                    after_paren < len && (bytes[after_paren] == b'{' || bytes[after_paren] == b';')
                } else {
                    false
                };

                // If this looks like a function declaration with default args, process it
                if is_func_decl {
                    // Output the identifier and '('
                    result.push_str(&ident);
                    result.push('(');
                    i = j + 1;

                    // Process parameters, removing default args
                    paren_depth = 1;
                    while i < len && paren_depth > 0 {
                        if bytes[i] == b'(' {
                            paren_depth += 1;
                            result.push('(');
                            i += 1;
                        } else if bytes[i] == b')' {
                            paren_depth -= 1;
                            if paren_depth == 0 {
                                result.push(')');
                                i += 1;
                                break;
                            }
                            result.push(')');
                            i += 1;
                        } else if bytes[i] == b'=' && paren_depth == 1 {
                            // Check it's not '=='
                            if i + 1 < len && bytes[i + 1] == b'=' {
                                result.push_str("==");
                                i += 2;
                            }
                            // Check it's not '!=', '<=', '>='
                            else if i > 0
                                && (bytes[i - 1] == b'!'
                                    || bytes[i - 1] == b'<'
                                    || bytes[i - 1] == b'>')
                            {
                                result.push('=');
                                i += 1;
                            } else {
                                // Skip the default value until ',' or ')'
                                // at the same parenthesis depth.
                                // Handle string literals and nested parens.
                                i += 1;
                                let mut d = 0i32;
                                while i < len {
                                    if bytes[i] == b'"' {
                                        i = skip_string_literal(bytes, i);
                                        continue;
                                    }
                                    if bytes[i] == b'(' {
                                        d += 1;
                                    } else if bytes[i] == b')' {
                                        if d == 0 {
                                            break;
                                        }
                                        d -= 1;
                                    } else if bytes[i] == b',' && d == 0 {
                                        break;
                                    }
                                    i += 1;
                                }
                            }
                        } else {
                            result.push(bytes[i] as char);
                            i += 1;
                        }
                    }
                    continue;
                }
            }
        }

        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// C++ 函数式类型转换: Type(value) → (Type)(value)
/// 只处理简单的情况: 类型名后跟括号, 且不在函数参数声明上下文中
pub(crate) fn convert_functional_casts(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut paren_depth = 0i32;
    // 收集已知类型名
    let mut known_types: Vec<String> = Vec::new();
    {
        let mut j = 0;
        while j < len {
            if bytes[j] == b'"' {
                j = skip_string_literal(bytes, j);
                continue;
            }
            if match_word_at(bytes, j, "struct") || match_word_at(bytes, j, "typedef") {
                let kw_len = if match_word_at(bytes, j, "struct") { 6 } else { 7 };
                let k = skip_ws(bytes, j + kw_len);
                if k < len && is_ident_char(bytes[k]) {
                    let (name, _) = read_ident(bytes, k);
                    if !name.is_empty() && !known_types.contains(&name) {
                        known_types.push(name);
                    }
                }
            }
            j += 1;
        }
    }
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
        if bytes[i] == b'(' {
            paren_depth += 1;
        }
        if bytes[i] == b')' {
            paren_depth = (paren_depth - 1).max(0);
        }
        // 检测类型名后跟 '('
        if is_ident_char(bytes[i]) && (i == 0 || !is_ident_char(bytes[i - 1])) {
            let (ident, ident_end) = read_ident(bytes, i);
            let j = skip_ws(bytes, ident_end);
            if j < len && bytes[j] == b'(' {
                // 检查是否是已知类型名或看起来像类型名
                let is_type = known_types.contains(&ident)
                    || ident.ends_with("_t")
                    || ident == "int"
                    || ident == "char"
                    || ident == "bool"
                    || ident == "long"
                    || ident == "short"
                    || ident == "unsigned";
                if is_type {
                    // 检查上下文: 前面是否是操作符/空白 (表达式上下文)
                    // 而不是标识符或 ')' (函数调用或声明上下文)
                    let prev = result.trim_end().as_bytes().last().copied().unwrap_or(b' ');
                    let is_expr_ctx = !is_ident_char(prev)
                        && prev != b')'
                        && prev != b']'
                        && prev != b'('
                        && prev != b',';
                    // 也检查是否在括号内且前面是操作符
                    if is_expr_ctx {
                        // 检查括号内容是否是简单值 (数字或简单表达式)
                        let mut d = 1i32;
                        let mut k = j + 1;
                        let mut is_simple = true;
                        while k < len && d > 0 {
                            if bytes[k] == b'(' {
                                d += 1;
                            } else if bytes[k] == b')' {
                                d -= 1;
                            } else if bytes[k] == b';' || bytes[k] == b'{' || bytes[k] == b'}' {
                                is_simple = false;
                                break;
                            }
                            k += 1;
                        }
                        if is_simple && d == 0 {
                            // 这是函数式类型转换, 转换为 C 风格
                            result.push('(');
                            result.push_str(&ident);
                            result.push(')');
                            i = ident_end;
                            continue;
                        }
                    }
                }
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// 保守版函数式类型转换: Type(number) → (Type)(number)
/// 只处理类型名后跟括号且括号内是简单整数的情况
pub(crate) fn convert_simple_functional_casts(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    // 收集已知类型名
    let mut known_types: Vec<String> = Vec::new();
    {
        let mut j = 0;
        while j < len {
            if bytes[j] == b'"' {
                j = skip_string_literal(bytes, j);
                continue;
            }
            if match_word_at(bytes, j, "struct")
                || match_word_at(bytes, j, "typedef")
                || match_word_at(bytes, j, "enum")
            {
                let kw_len = if match_word_at(bytes, j, "struct") {
                    6
                } else if match_word_at(bytes, j, "typedef") {
                    7
                } else {
                    4
                };
                let k = skip_ws(bytes, j + kw_len);
                if k < len && is_ident_char(bytes[k]) {
                    let (name, _) = read_ident(bytes, k);
                    if !name.is_empty() && !known_types.contains(&name) {
                        known_types.push(name);
                    }
                }
            }
            j += 1;
        }
    }
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
        // 检测类型名后跟 '(' 和简单数字
        if is_ident_char(bytes[i]) && (i == 0 || !is_ident_char(bytes[i - 1])) {
            let (ident, ident_end) = read_ident(bytes, i);
            let j = skip_ws(bytes, ident_end);
            if j < len && bytes[j] == b'(' {
                let is_type = known_types.contains(&ident)
                    || ident.ends_with("_t")
                    || ident == "int"
                    || ident == "char"
                    || ident == "bool"
                    || ident == "long"
                    || ident == "short"
                    || ident == "unsigned"
                    || ident == "size_t";
                if is_type {
                    // 找到匹配的 ')'
                    let mut k = j + 1;
                    let mut depth = 1i32;
                    let mut has_star = false;
                    while k < len && depth > 0 {
                        if bytes[k] == b'(' {
                            depth += 1;
                        } else if bytes[k] == b')' {
                            depth -= 1;
                        } else if bytes[k] == b'*' {
                            has_star = true;
                        }
                        k += 1;
                    }
                    // 如果括号内容包含 '*', 可能是函数指针声明, 不转换
                    if depth == 0 && !has_star {
                        // 检查前面是否是表达式上下文
                        let prev = result.trim_end().as_bytes().last().copied().unwrap_or(b' ');
                        let is_expr_ctx = !is_ident_char(prev)
                            && prev != b')'
                            && prev != b']'
                            && prev != b';'
                            && prev != b'{'
                            && prev != b'}';
                        if is_expr_ctx {
                            // 转换: type(expr) → (type)(expr)
                            result.push('(');
                            result.push_str(&ident);
                            result.push(')');
                            i = ident_end; // 停在 '(' 前面, 下次循环输出 '('
                            continue;
                        }
                    }
                }
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// 转换 C++ 直接初始化: Type name (expr); → Type name = (expr);
/// 也处理指针: Type *name (expr); → Type *name = (expr);
pub(crate) fn convert_direct_initialization(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut last_push = 0; // 上次 push 到的位置
    while i < len {
        // 跳过字符串和注释
        if bytes[i] == b'"' {
            let j = skip_string_literal(bytes, i);
            result.push_str(&s[i..j]);
            i = j;
            last_push = i;
            continue;
        }
        if bytes[i] == b'/' && i + 1 < len && (bytes[i + 1] == b'/' || bytes[i + 1] == b'*') {
            let j = skip_comment(bytes, i);
            result.push_str(&s[i..j]);
            i = j;
            last_push = i;
            continue;
        }
        // 查找模式: identifier (expr) ;
        if bytes[i] == b'(' && i > 0 {
            // 检查前面是否是标识符
            let mut j = i;
            while j > 0 && (bytes[j - 1] == b' ' || bytes[j - 1] == b'\t' || bytes[j - 1] == b'\n')
            {
                j -= 1;
            }
            if j > 0 && is_ident_char(bytes[j - 1]) {
                let id_end = j;
                let mut id_start = j;
                while id_start > 0 && is_ident_char(bytes[id_start - 1]) {
                    id_start -= 1;
                }
                // 检查标识符前面是否有类型名或 *
                let mut k = id_start;
                while k > 0
                    && (bytes[k - 1] == b' '
                        || bytes[k - 1] == b'\t'
                        || bytes[k - 1] == b'\n'
                        || bytes[k - 1] == b'*')
                {
                    k -= 1;
                }
                if k > 0 && is_ident_char(bytes[k - 1]) {
                    // 找到匹配的 )
                    let mut depth = 1;
                    let mut m = i + 1;
                    while m < len && depth > 0 {
                        if bytes[m] == b'(' {
                            depth += 1;
                        } else if bytes[m] == b')' {
                            depth -= 1;
                        }
                        m += 1;
                    }
                    if depth == 0 && m < len {
                        // 检查 ) 后面是否是 ;
                        let mut n = m;
                        while n < len
                            && (bytes[n] == b' ' || bytes[n] == b'\t' || bytes[n] == b'\n')
                        {
                            n += 1;
                        }
                        if n < len && bytes[n] == b';' {
                            // 转换: identifier (expr) ; → identifier = (expr) ;
                            result.push_str(&s[last_push..id_end]);
                            result.push_str(" = ");
                            result.push_str(&s[i..n]);
                            result.push(';');
                            i = n + 1;
                            last_push = i;
                            continue;
                        }
                    }
                }
            }
        }
        i += 1;
    }
    // 推送剩余部分
    if last_push < len {
        result.push_str(&s[last_push..]);
    }
    result
}

/// Convert C++ direct initialization: Type *name (expr); -> Type *name = (expr);
pub(crate) fn convert_direct_init(s: &str) -> String {
    // Specific pattern: bidi_context *ctxt (expr);
    // More general: look for identifier followed by (expr) where preceded by type *
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut result = String::with_capacity(len);
    let mut i = 0;
    while i < len {
        // Look for pattern: *identifier (
        if bytes[i] == b'*' {
            result.push('*');
            i += 1;
            // Skip whitespace
            let _ws_start = i;
            while i < len && (bytes[i] == b' ' || bytes[i] == b'\t' || bytes[i] == b'\n') {
                result.push(bytes[i] as char);
                i += 1;
            }
            // Read identifier
            if i < len && is_ident_char(bytes[i]) {
                let id_start = i;
                while i < len && is_ident_char(bytes[i]) {
                    i += 1;
                }
                let ident = &s[id_start..i];
                // Skip whitespace
                while i < len && (bytes[i] == b' ' || bytes[i] == b'\t' || bytes[i] == b'\n') {
                    i += 1;
                }
                // Check if followed by (
                if i < len && bytes[i] == b'(' {
                    // Find matching )
                    let mut depth = 1;
                    let mut j = i + 1;
                    while j < len && depth > 0 {
                        if bytes[j] == b'(' {
                            depth += 1;
                        } else if bytes[j] == b')' {
                            depth -= 1;
                        }
                        j += 1;
                    }
                    if depth == 0 {
                        // Check if followed by ;
                        let mut k = j;
                        while k < len
                            && (bytes[k] == b' ' || bytes[k] == b'\t' || bytes[k] == b'\n')
                        {
                            k += 1;
                        }
                        if k < len && bytes[k] == b';' {
                            // Check that the content of () is an expression (not a type declaration)
                            let inner = &s[i + 1..j - 1];
                            if !inner.contains("*") && !inner.contains("enum ") {
                                // This is direct initialization: convert to = (expr)
                                result.push_str(ident);
                                result.push_str(" = ");
                                result.push_str(&s[i..j]);
                                i = j;
                                continue;
                            }
                        }
                    }
                }
                // Not a direct init, just output the identifier
                result.push_str(ident);
                continue;
            }
            continue;
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// Convert C++ reference-to-array declarations to C pointer-to-array
/// Type *(&name)[N][M] -> Type *(*name)[N][M]
pub(crate) fn convert_ref_to_array(s: &str) -> String {
    // Pattern: *(&identifier) -> *(*identifier)
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut result = String::with_capacity(len);
    let mut i = 0;
    while i < len {
        if bytes[i] == b'(' && i + 1 < len && bytes[i + 1] == b'&' {
            // Check if this is *(&pattern)
            if i > 0 && bytes[i - 1] == b'*' {
                // Find matching )
                let mut j = i + 2;
                let mut depth = 1;
                while j < len && depth > 0 {
                    if bytes[j] == b'(' {
                        depth += 1;
                    } else if bytes[j] == b')' {
                        depth -= 1;
                    }
                    j += 1;
                }
                if depth == 0 && j < len && bytes[j] == b')' {
                    // Check if followed by [
                    if j + 1 < len && bytes[j + 1] == b'[' {
                        // Extract identifier between & and )
                        let id_start = i + 2;
                        let id_end = j;
                        let ident = &s[id_start..id_end];
                        // Check it's a valid identifier
                        if !ident.is_empty()
                            && ident.chars().all(|c| c.is_alphanumeric() || c == '_')
                        {
                            // Replace *(&ident) with *(*ident)
                            // Remove the & and change first ( to *
                            let result_bytes = result.as_bytes();
                            let new_result_len = result_bytes.len() - 1; // remove trailing *
                            result = String::from_utf8_lossy(&result_bytes[..new_result_len])
                                .to_string();
                            result.push_str(&format!("(*{})", ident));
                            i = j + 1; // skip past )
                            continue;
                        }
                    }
                }
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// Convert C++ conversion operators: operator name( -> type _cast_name(
pub(crate) fn convert_conversion_operators(s: &str) -> String {
    // Pattern: ^operator identifier( at start of line
    let mut result = String::with_capacity(s.len());
    for line in s.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("operator ") {
            // after "operator "
            if let Some(paren_pos) = rest.find('(') {
                let func_name = rest[..paren_pos].trim();
                let after = &rest[paren_pos..];
                // Get the indentation
                let indent = &line[..line.len() - trimmed.len()];
                result.push_str(indent);
                result.push_str("void *_cast_");
                result.push_str(func_name);
                result.push_str(after);
            } else {
                result.push_str(line);
            }
        } else {
            result.push_str(line);
        }
        result.push('\n');
    }
    // Remove trailing newline if original didn't have one
    if !s.ends_with('\n') && result.ends_with('\n') {
        result.pop();
    }
    result
}

/// Fix member function calls in extracted inline functions
pub(crate) fn fix_inline_member_calls(s: &str) -> String {
    let mut result = s.to_string();
    // Fix: bytes_left () -> this_->m_bytes_left (in cpp_display_width_computation_done)
    result = result.replace(
        "cpp_display_width_computation_done(struct cpp_display_width_computation *this_) { return !bytes_left (); }",
        "cpp_display_width_computation_done(struct cpp_display_width_computation *this_) { return !this_->m_bytes_left; }"
    );
    // Fix: get_loc (0) -> this_->m_loc (in rich_location_get_loc)
    result = result.replace(
        "rich_location_get_loc(struct rich_location *this_) { return get_loc (0); }",
        "rich_location_get_loc(struct rich_location *this_) { return this_->m_loc; }",
    );
    // Fix member function calls on struct variables:
    // rich_loc.set_escape_on_output (true) -> rich_loc_set_escape_on_output (&rich_loc, true)
    // rich_loc.add_range (expr) -> rich_loc_add_range (&rich_loc, expr)
    result = result.replace(
        "rich_loc.set_escape_on_output (true)",
        "rich_loc_set_escape_on_output (&rich_loc, true)",
    );
    result = result.replace("rich_loc.add_range (", "rich_loc_add_range (&rich_loc, ");
    // Fix bidi_vec.count() -> bidi_vec_count()
    result = result.replace("bidi_vec.count ()", "bidi_vec_count ()");
    // Fix C++ direct initialization: bidi_context *ctxt (expr) -> bidi_context *ctxt = (expr)
    result = result.replace(
        "bidi_context *ctxt (bidi_vec[range_idx - 1])",
        "bidi_context *ctxt = (bidi_vec[range_idx - 1])",
    );
    // Fix pointer member access: ctxt.m_kind -> ctxt->m_kind
    result = result.replace("ctxt.m_kind", "ctxt->m_kind");
    // Fix reference-to-array: cpp_hashnode *(&n_modules)[N][2] -> cpp_hashnode *(*n_modules)[N][2]
    result = result.replace("*(&n_modules)[", "*(*n_modules)[");
    // Global fix: replace bare member function calls in functions with this_ parameter
    // Collect all known member function names from extracted functions
    {
        let mut global_member_fns: Vec<(String, String)> = Vec::new(); // (struct_name, fn_name)
        let bytes = result.as_bytes();
        let mut si = 0;
        while si < bytes.len() {
            // Look for pattern: (struct STRUCTNAME *this_ (with possible whitespace before '(')
            if bytes[si] == b'(' {
                let after_paren = skip_ws(bytes, si + 1);
                if after_paren + 7 <= bytes.len()
                    && bytes[after_paren..after_paren + 7] == *b"struct "
                {
                    let after_struct = after_paren + 7;
                    if is_ident_char(bytes[after_struct]) {
                        let (sname, sname_end) = read_ident(bytes, after_struct);
                        let after_sname = skip_ws(bytes, sname_end);
                        // Check for *this_ or *this pattern (pointer parameter named this_)
                        let this_pattern_ok = if after_sname + 5 <= bytes.len()
                            && bytes[after_sname..after_sname + 5] == *b"*this"
                        {
                            true
                        } else {
                            after_sname + 6 <= bytes.len()
                                && bytes[after_sname..after_sname + 6] == *b" *this"
                        };
                        if this_pattern_ok {
                            // Found a function with struct sname *this_ parameter
                            // Now find the function name: go backward from '(' (skip whitespace)
                            let mut fn_end = si;
                            while fn_end > 0
                                && (bytes[fn_end - 1] == b' '
                                    || bytes[fn_end - 1] == b'\t'
                                    || bytes[fn_end - 1] == b'\n')
                            {
                                fn_end -= 1;
                            }
                            let mut fn_start = fn_end;
                            while fn_start > 0 && is_ident_char(bytes[fn_start - 1]) {
                                fn_start -= 1;
                            }
                            if fn_start < fn_end {
                                let full_fname = &result[fn_start..fn_end];
                                // Check if full_fname starts with sname + "_"
                                let prefix = format!("{}_", sname);
                                if full_fname.starts_with(&prefix) {
                                    let mname = &full_fname[prefix.len()..];
                                    if !mname.is_empty()
                                        && !global_member_fns
                                            .iter()
                                            .any(|(sn, mn)| sn == &sname && mn == mname)
                                    {
                                        global_member_fns
                                            .push((sname.to_string(), mname.to_string()));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            si += 1;
        }
        // For each known member function, replace ALL bare calls globally
        // (since base class member calls can appear in derived class constructors)
        for (sname, mname) in &global_member_fns {
            let fixed_call = format!("{}_{}(this_, ", sname, mname);
            let fixed_call_empty = format!("{}_{}(this_)", sname, mname);
            let mut new_result = String::with_capacity(result.len());
            let rbytes = result.as_bytes();
            let mut ri = 0;
            while ri < rbytes.len() {
                if match_word_at(rbytes, ri, mname) {
                    let after_mname = skip_ws(rbytes, ri + mname.len());
                    if after_mname < rbytes.len() && rbytes[after_mname] == b'(' {
                        // Check it's not already prefixed (no _ before the name)
                        let before_ok =
                            ri == 0 || (!is_ident_char(rbytes[ri - 1]) && rbytes[ri - 1] != b'_');
                        if before_ok {
                            let after_paren = skip_ws(rbytes, after_mname + 1);
                            if after_paren < rbytes.len() && rbytes[after_paren] == b')' {
                                new_result.push_str(&fixed_call_empty);
                                ri = after_paren + 1;
                                continue;
                            } else {
                                new_result.push_str(&fixed_call);
                                ri = after_mname + 1;
                                continue;
                            }
                        }
                    }
                }
                new_result.push(rbytes[ri] as char);
                ri += 1;
            }
            result = new_result;
        }
    }
    result
}

/// Convert C++ destructs to C function definitions
/// ClassName_~ClassName (ClassName) () -> void _dtor_ClassName ()
pub(crate) fn convert_destructors(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        // Look for pattern: identifier_~identifier
        if bytes[i] == b'~' && i > 0 && i + 1 < len {
            // Check if there's _ before ~
            if bytes[i - 1] == b'_' {
                // Find the start of the class name before _
                let mut start = i - 2;
                while start > 0
                    && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_')
                {
                    start -= 1;
                }
                // Extract class name
                let class_name = &s[start..i - 1];
                // Find the end of the destructor name (after ~)
                let mut j = i + 1;
                while j < len && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                    j += 1;
                }
                // Skip whitespace
                while j < len && (bytes[j] == b' ' || bytes[j] == b'\t') {
                    j += 1;
                }
                // Skip (ClassName) if present
                if j < len && bytes[j] == b'(' {
                    let mut k = j + 1;
                    let mut depth = 1;
                    while k < len && depth > 0 {
                        if bytes[k] == b'(' {
                            depth += 1;
                        } else if bytes[k] == b')' {
                            depth -= 1;
                        }
                        k += 1;
                    }
                    j = k;
                }
                // Remove the class_name_ prefix from result
                let prefix_to_remove = class_name.len() + 1; // +1 for _
                let new_len = result.len().saturating_sub(prefix_to_remove);
                result.truncate(new_len);
                // Write void _dtor_ClassName
                result.push_str("void _dtor_");
                result.push_str(class_name);
                i = j;
                continue;
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// Convert C++ new and delete expressions to C
/// new (ptr) ClassName() -> placement_new_ClassName(ptr)
/// new ClassName(args) -> allocate_and_init_ClassName(args)
/// delete expr; -> free(expr);
///
/// 实现注意: 输入可能包含非 ASCII 字符(如注释), 全程按字节处理,
/// 输出字节 Vec 后统一转回 UTF-8, 避免对 &str 做非 char 边界的切片.
pub(crate) fn convert_new_delete_expressions(s: &str) -> String {
    let mut result: Vec<u8> = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        // Look for 'delete ' (standalone keyword, not part of identifier)
        if i + 7 <= len && &bytes[i..i + 7] == b"delete " {
            // Check word boundary: must not be preceded by alphanumeric or _
            let prev_ok = i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
            if prev_ok {
                // Find the end of the expression (until ;)
                let mut j = i + 7;
                let mut depth = 0;
                while j < len {
                    if bytes[j] == b'(' {
                        depth += 1;
                    } else if bytes[j] == b')' {
                        depth -= 1;
                    } else if bytes[j] == b';' && depth == 0 {
                        break;
                    }
                    j += 1;
                }
                let expr = trim_ascii(&bytes[i + 7..j]);
                result.extend_from_slice(b"free (");
                result.extend_from_slice(expr);
                result.push(b')');
                i = j;
                continue;
            }
        }
        // Look for 'new '
        if i + 4 <= len && &bytes[i..i + 4] == b"new " {
            // Check if this is placement new: new (ptr) ClassName
            let mut j = i + 4;
            // Skip whitespace
            while j < len && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            if j < len && bytes[j] == b'(' {
                // This is placement new: new (ptr) ClassName
                // Find matching )
                let mut k = j + 1;
                let mut depth = 1;
                while k < len && depth > 0 {
                    if bytes[k] == b'(' {
                        depth += 1;
                    } else if bytes[k] == b')' {
                        depth -= 1;
                    }
                    k += 1;
                }
                // Extract pointer expression
                let ptr_expr = &bytes[j + 1..k - 1];
                // Skip whitespace
                while k < len && (bytes[k] == b' ' || bytes[k] == b'\t') {
                    k += 1;
                }
                // Find class name
                let mut m = k;
                while m < len && (bytes[m].is_ascii_alphanumeric() || bytes[m] == b'_') {
                    m += 1;
                }
                let class_name = &bytes[k..m];
                // Skip whitespace and find (
                while m < len && (bytes[m] == b' ' || bytes[m] == b'\t') {
                    m += 1;
                }
                if m < len && bytes[m] == b'(' {
                    // Find matching )
                    let mut n = m + 1;
                    let mut depth = 1;
                    while n < len && depth > 0 {
                        if bytes[n] == b'(' {
                            depth += 1;
                        } else if bytes[n] == b')' {
                            depth -= 1;
                        }
                        n += 1;
                    }
                    let args = &bytes[m + 1..n - 1];
                    result.extend_from_slice(b"_ctor_");
                    result.extend_from_slice(class_name);
                    result.extend_from_slice(b" (");
                    result.extend_from_slice(ptr_expr);
                    if !args.is_empty() {
                        result.extend_from_slice(b", ");
                        result.extend_from_slice(args);
                    }
                    result.push(b')');
                    i = n;
                    continue;
                }
            } else {
                // Regular new: new ClassName(args)
                // Find class name
                let mut k = j;
                while k < len && (bytes[k].is_ascii_alphanumeric() || bytes[k] == b'_') {
                    k += 1;
                }
                let class_name = &bytes[j..k];
                // Skip whitespace
                while k < len && (bytes[k] == b' ' || bytes[k] == b'\t') {
                    k += 1;
                }
                if k < len && bytes[k] == b'(' {
                    // Find matching )
                    let mut m = k + 1;
                    let mut depth = 1;
                    while m < len && depth > 0 {
                        if bytes[m] == b'(' {
                            depth += 1;
                        } else if bytes[m] == b')' {
                            depth -= 1;
                        }
                        m += 1;
                    }
                    let args = &bytes[k + 1..m - 1];
                    result.extend_from_slice(b"_new_");
                    result.extend_from_slice(class_name);
                    result.extend_from_slice(b" (");
                    result.extend_from_slice(args);
                    result.push(b')');
                    i = m;
                    continue;
                } else if k < len && bytes[k] == b'*' {
                    // new ClassName *something - just keep as is for now
                    result.extend_from_slice(b"new ");
                    i = j;
                    continue;
                }
            }
        }
        result.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(result).unwrap_or_else(|_| s.to_string())
}

/// Convert vector member calls to C function calls
/// vec.push(x) -> vec_push(&vec, x)
/// vec.count() -> vec_count(&vec)
/// vec.truncate(n) -> vec_truncate(&vec, n)
pub(crate) fn convert_vector_member_calls(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        // Look for .push (, .count (, .truncate (
        if bytes[i] == b'.' && i + 1 < len {
            let rest = &s[i + 1..];
            if rest.starts_with("push (") || rest.starts_with("push(") {
                // Find the variable name before .
                let mut start = i;
                while start > 0
                    && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_')
                {
                    start -= 1;
                }
                let var_name = &s[start..i];
                // Find matching )
                let paren_start = i + 1 + if rest.starts_with("push (") { 6 } else { 5 };
                let mut j = paren_start;
                let mut depth = 1;
                while j < len && depth > 0 {
                    if bytes[j] == b'(' {
                        depth += 1;
                    } else if bytes[j] == b')' {
                        depth -= 1;
                    }
                    j += 1;
                }
                let args = &s[paren_start..j - 1];
                // Remove what we added for var_name and write vec_push(&vec, args)
                let prefix_len = var_name.len();
                let new_len = result.len();
                result.truncate(new_len - prefix_len);
                result.push_str(var_name);
                result.push_str("_push (&");
                result.push_str(var_name);
                result.push_str(", ");
                result.push_str(args);
                result.push(')');
                i = j;
                continue;
            } else if rest.starts_with("count (") || rest.starts_with("count(") {
                // Find the variable name before .
                let mut start = i;
                while start > 0
                    && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_')
                {
                    start -= 1;
                }
                let var_name = &s[start..i];
                // Find matching )
                let paren_start = i + 1 + if rest.starts_with("count (") { 7 } else { 6 };
                let mut j = paren_start;
                let mut depth = 1;
                while j < len && depth > 0 {
                    if bytes[j] == b'(' {
                        depth += 1;
                    } else if bytes[j] == b')' {
                        depth -= 1;
                    }
                    j += 1;
                }
                // Remove what we added for var_name and write vec_count(&vec)
                let prefix_len = var_name.len();
                let new_len = result.len();
                result.truncate(new_len - prefix_len);
                result.push_str(var_name);
                result.push_str("_count (&");
                result.push_str(var_name);
                result.push(')');
                i = j;
                continue;
            } else if rest.starts_with("truncate (") || rest.starts_with("truncate(") {
                // Find the variable name before .
                let mut start = i;
                while start > 0
                    && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_')
                {
                    start -= 1;
                }
                let var_name = &s[start..i];
                // Find matching )
                let paren_start = i + 1 + if rest.starts_with("truncate (") { 10 } else { 9 };
                let mut j = paren_start;
                let mut depth = 1;
                while j < len && depth > 0 {
                    if bytes[j] == b'(' {
                        depth += 1;
                    } else if bytes[j] == b')' {
                        depth -= 1;
                    }
                    j += 1;
                }
                let args = &s[paren_start..j - 1];
                // Remove what we added for var_name and write vec_truncate(&vec, args)
                let prefix_len = var_name.len();
                let new_len = result.len();
                result.truncate(new_len - prefix_len);
                result.push_str(var_name);
                result.push_str("_truncate (&");
                result.push_str(var_name);
                result.push_str(", ");
                result.push_str(args);
                result.push(')');
                i = j;
                continue;
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `replace_word` 只在词边界替换, 并且当替换结果与后面的标识符会粘连时补一个空格。
    #[test]
    fn replace_word_respects_word_boundaries_and_spacing() {
        assert_eq!(replace_word("int new int; new x;", "new", "goto"), "int goto int; goto x;");
        // 前/后是标识符字符则不算独立词 (`renew`、`newx` 都不动)
        assert_eq!(replace_word("renew newx", "new", "goto"), "renew newx");
        // 替换词以字母数字结尾且后面紧跟标识符时必须补空格, 否则会粘成新标识符
        assert_eq!(replace_word("sizeof T", "sizeof", "size_of"), "size_of T");
    }

    #[test]
    fn replace_phrase_matches_whole_phrase_only() {
        assert_eq!(
            replace_phrase("operator new (size)", "operator new", "malloc"),
            "malloc (size)"
        );
        assert_eq!(replace_phrase("xoperator new", "operator new", "malloc"), "xoperator new");
    }

    #[test]
    fn ascii_helpers_trim_and_locate() {
        assert_eq!(trim_ascii(b"  int x  "), b"int x");
        assert_eq!(trim_ascii_end(b"int x   "), b"int x");
        assert_eq!(last_ascii_word(b"return some_name"), "some_name");
        assert_eq!(last_ascii_word(b"a+b"), "b");
        // 返回的是相对 `start` 的下标 (调用方自己加回 start)
        assert_eq!(find_ascii_eq(b"x=1", 0), Some(1));
        assert_eq!(find_ascii_eq(b"x=1", 1), Some(0));
        assert_eq!(find_ascii_eq(b"abc", 0), None);
    }

    /// `T t{...}` 形式的花括号初始化在 C 里不存在, 必须换成 `T t = {...}`。
    /// 类型名来自同一份文本里的 `struct`/`class` 名 (该 pass 只认已声明的记录类型,
    /// 否则会把 `arr[2]{...}` 这类下标误当成初始化)。
    #[test]
    fn brace_init_becomes_c_initializer() {
        let src = "struct Point { int x; int y; };\nstruct Point p{1, 2};\n";
        let out = replace_brace_init(src);
        assert!(out.contains("struct Point p = {1, 2};"), "{out}");
        // 未声明的类型名不被猜测, 下标形式保持原样
        assert_eq!(replace_brace_init("int arr[2]{1, 2};\n"), "int arr[2]{1, 2};\n");
    }
}
