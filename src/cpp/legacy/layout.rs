//! Legacy class layout: C 文本层的 struct/基类扁平化与成员表模拟。
//!
//! Stage 1 拆分: 从 `lower.rs` 原样搬来, 行为不变。职责边界: 只处理「类/结构体的
//! 声明区形态」—— 继承展开成字段、结构体体清理、`struct` 前缀补齐、成员表收集、
//! 前置声明与缺失 `struct` 关键字的修补。native path 的布局由 `cpp::abi_layout` 计算,
//! 不经过本模块。

use crate::cpp::legacy::{
    decl_cleanup::*, expr::*, name_flatten::*, runtime::*, template_text::*, text_scan::*,
};

/// 删除 struct/struct 定义中的继承语法: `struct X : Y {` → `struct X {`
/// 仅删除 ':' 到 '{' 之间的继承列表, 其他内容原样保留.
pub(crate) fn remove_struct_inheritance(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
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
        // 检测 struct 或 class 关键字
        if match_word_at(bytes, i, "struct") || match_word_at(bytes, i, "class") {
            let kw_len = if match_word_at(bytes, i, "struct") { 6 } else { 5 };
            // 输出关键字
            result.push_str(&s[i..i + kw_len]);
            let mut j = skip_ws(bytes, i + kw_len);
            // 读取结构名 (如果有)
            if j < len && is_ident_char(bytes[j]) {
                let (_, name_end) = read_ident(bytes, j);
                // 输出从关键字后到 name_end 的所有内容 (含空白和结构名)
                result.push_str(&s[i + kw_len..name_end]);
                j = skip_ws(bytes, name_end);
                // 检查是否有 ':' (继承) 且不是 '::'
                if j < len && bytes[j] == b':' && (j + 1 >= len || bytes[j + 1] != b':') {
                    // 跳过继承列表直到 '{' 或 ';'
                    while j < len && bytes[j] != b'{' && bytes[j] != b';' {
                        j += 1;
                    }
                    // 不输出继承部分, 继续从 '{' 或 ';' 开始
                    i = j;
                    continue;
                } else {
                    // 没有继承, 从 name_end 继续 (保留空白)
                    i = name_end;
                    continue;
                }
            }
            i += kw_len;
            continue;
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// 清理 struct/struct 定义体: 删除构造函数/析构函数声明, = delete,
/// operator=, 默认参数, static 成员初始化, 内联成员函数定义,
/// 模板字段类型, mutable 等.
pub(crate) fn clean_struct_bodies(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    // 跟踪 struct 名栈, 用于识别构造函数
    let mut struct_name_stack: Vec<String> = Vec::new();
    // 收集内联成员函数, 在 struct 体结束后输出
    let mut pending_functions: Vec<String> = Vec::new();
    // 收集当前 struct 的字段名, 用于在 inline 函数体中添加 this_-> 前缀
    let mut field_names: Vec<String> = Vec::new();
    // 收集当前 struct 的成员函数名, 用于转换裸成员函数调用
    let mut member_fn_names: Vec<String> = Vec::new();
    // 跟踪括号深度 (用于区分 struct 定义和 struct 类型参数)
    let mut paren_depth = 0i32;
    // 跟踪 struct 体内嵌套大括号深度 (enum/嵌套块等)
    let mut nested_brace_depth = 0i32;

    while i < len {
        // 跳过字符串
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

        // 跟踪括号深度
        if bytes[i] == b'(' {
            paren_depth += 1;
            result.push('(');
            i += 1;
            continue;
        }
        if bytes[i] == b')' {
            paren_depth -= 1;
            result.push(')');
            i += 1;
            continue;
        }

        // 检测 struct/struct NAME 开始 (只在不在括号内时检测)
        // 允许嵌套的 struct 定义: 即使 struct_name_stack 不为空也要检测
        if (match_word_at(bytes, i, "struct") || match_word_at(bytes, i, "class"))
            && paren_depth == 0
        {
            let kw = if match_word_at(bytes, i, "struct") {
                "struct"
            } else {
                "class"
            };
            let kw_len = kw.len();
            let mut j = skip_ws(bytes, i + kw_len);
            if j < len && is_ident_char(bytes[j]) {
                let (name, name_end) = read_ident(bytes, j);
                j = skip_ws(bytes, name_end);
                // Check if this is a struct DEFINITION (struct NAME {)
                // vs a variable declaration (struct NAME var = { ... })
                // After the name, only whitespace, attributes, or
                // inheritance (: base) should appear before '{'.
                // If we see '=', '[', or another identifier before '{',
                // it's a variable declaration, not a struct definition.
                let is_struct_def = if j < len && bytes[j] == b'{' {
                    true
                } else if j < len && bytes[j] == b':' {
                    // Inheritance: struct NAME : base { ... }
                    // Skip to '{'
                    let mut k = j;
                    while k < len && bytes[k] != b'{' && bytes[k] != b';' {
                        k += 1;
                    }
                    k < len && bytes[k] == b'{'
                } else {
                    false
                };
                if is_struct_def {
                    // Skip to '{'
                    while j < len && bytes[j] != b'{' && bytes[j] != b';' {
                        j += 1;
                    }
                    if j < len && bytes[j] == b'{' {
                        struct_name_stack.push(name.clone());
                        nested_brace_depth = 0; // 重置嵌套深度
                        result.push_str(&s[i..j + 1]);
                        i = j + 1;
                        // Pre-scan struct body to collect field names
                        // for adding this_-> prefix in inline member functions
                        field_names.clear();
                        {
                            let mut scan = i;
                            let mut scan_depth = 1i32;
                            while scan < len && scan_depth > 0 {
                                if bytes[scan] == b'"' {
                                    scan = skip_string_literal(bytes, scan);
                                    continue;
                                }
                                if bytes[scan] == b'{' {
                                    scan_depth += 1;
                                    scan += 1;
                                    continue;
                                }
                                if bytes[scan] == b'}' {
                                    scan_depth -= 1;
                                    scan += 1;
                                    continue;
                                }
                                if bytes[scan] == b';' && scan_depth == 1 {
                                    // Look backward for field name: last identifier before ';'
                                    let mut fe = scan;
                                    while fe > i && !is_ident_char(bytes[fe - 1]) {
                                        fe -= 1;
                                    }
                                    let mut fs = fe;
                                    while fs > i && is_ident_char(bytes[fs - 1]) {
                                        fs -= 1;
                                    }
                                    if fs < fe {
                                        let fname = &s[fs..fe];
                                        // Skip keywords and type names
                                        if fname != "const"
                                            && fname != "volatile"
                                            && fname != "static"
                                            && fname != "inline"
                                            && fname != "virtual"
                                            && fname != "explicit"
                                            && fname != "struct"
                                            && fname != "class"
                                            && fname != "enum"
                                            && fname != "union"
                                            && fname != "void"
                                            && fname != "int"
                                            && fname != "char"
                                            && fname != "float"
                                            && fname != "double"
                                            && fname != "long"
                                            && fname != "short"
                                            && fname != "unsigned"
                                            && fname != "signed"
                                            && fname != "bool"
                                            && fname != "auto"
                                            && fname != "return"
                                            && fname != "delete"
                                            && fname != "default"
                                            && fname != "public"
                                            && fname != "private"
                                            && fname != "protected"
                                            && fname != "friend"
                                            && fname != "typedef"
                                            && fname != "namespace"
                                            && fname != "template"
                                            && fname != "typename"
                                            && fname != "using"
                                            && fname != "new"
                                            && fname != "delete"
                                            && fname != "this"
                                            && !fname.starts_with('~')
                                            && fname != name
                                            && !field_names.contains(&fname.to_string())
                                        {
                                            field_names.push(fname.to_string());
                                        }
                                    }
                                }
                                scan += 1;
                            }
                        }
                        // Pre-scan struct body to collect member function names
                        // for converting bare member calls in extracted bodies
                        member_fn_names.clear();
                        {
                            let mut scan = i;
                            let mut scan_depth = 1i32;
                            while scan < len && scan_depth > 0 {
                                if bytes[scan] == b'"' {
                                    scan = skip_string_literal(bytes, scan);
                                    continue;
                                }
                                if bytes[scan] == b'{' {
                                    scan_depth += 1;
                                    scan += 1;
                                    continue;
                                }
                                if bytes[scan] == b'}' {
                                    scan_depth -= 1;
                                    scan += 1;
                                    continue;
                                }
                                // Look for pattern: ident(...) ... {
                                if is_ident_char(bytes[scan])
                                    && (scan == 0 || !is_ident_char(bytes[scan - 1]))
                                {
                                    let (ident, ident_end) = read_ident(bytes, scan);
                                    if ident != "if" && ident != "while" && ident != "for"
                                    && ident != "switch" && ident != "catch" && ident != "return"
                                    && ident != "enum" && ident != "struct" && ident != "class"
                                    && ident != name.as_str() // not the constructor
                                    && !ident.starts_with('~')
                                    {
                                        let mut sk = skip_ws(bytes, ident_end);
                                        if sk < len && bytes[sk] == b'(' {
                                            // Skip parens
                                            let mut pd = 1i32;
                                            sk += 1;
                                            while sk < len && pd > 0 {
                                                if bytes[sk] == b'(' {
                                                    pd += 1;
                                                } else if bytes[sk] == b')' {
                                                    pd -= 1;
                                                }
                                                sk += 1;
                                            }
                                            sk = skip_ws(bytes, sk);
                                            // Skip const/noexcept/override
                                            while sk < len && bytes[sk] != b'{' && bytes[sk] != b';'
                                            {
                                                sk += 1;
                                            }
                                            if sk < len
                                                && bytes[sk] == b'{'
                                                && !member_fn_names.contains(&ident.to_string())
                                            {
                                                member_fn_names.push(ident.to_string());
                                            }
                                        }
                                    }
                                }
                                scan += 1;
                            }
                        }
                        continue;
                    }
                } // end if is_struct_def
            }
        }

        // 如果在 struct 体内
        if !struct_name_stack.is_empty() {
            let struct_name = struct_name_stack.last().unwrap().clone();

            // 跟踪嵌套大括号深度 (enum { ... }, 匿名 union { ... } 等)
            // 注意: 这里的 { 不是来自 struct/class 检测 (那个在上面的外层处理)
            if nested_brace_depth > 0 {
                // 在嵌套块内, 只跟踪大括号
                if bytes[i] == b'{' {
                    nested_brace_depth += 1;
                    result.push('{');
                    i += 1;
                    continue;
                }
                if bytes[i] == b'}' {
                    nested_brace_depth -= 1;
                    result.push('}');
                    i += 1;
                    continue;
                }
                // 在嵌套块内, 直接输出字符
                result.push(bytes[i] as char);
                i += 1;
                continue;
            }
            // nested_brace_depth == 0: 正常 struct 体处理

            // 跳过嵌套的 struct/union 定义: struct { ... } name; 或 union { ... } name;
            if (match_word_at(bytes, i, "struct") || match_word_at(bytes, i, "union"))
                && i + 6 < len
            {
                let kw = if match_word_at(bytes, i, "struct") {
                    "struct"
                } else {
                    "union"
                };
                let kw_len = kw.len();
                let mut j = skip_ws(bytes, i + kw_len);
                // 检查是否是匿名定义 (后面直接是 '{')
                if j < len && bytes[j] == b'{' {
                    // 跳过整个嵌套定义: { ... } name;
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
                    // 跳过变量名和 ';'
                    while j < len && bytes[j] != b';' {
                        j += 1;
                    }
                    if j < len {
                        j += 1;
                    }
                    // 输出整个嵌套定义
                    result.push_str(&s[i..j]);
                    i = j;
                    continue;
                }
            }

            // 检测 '{' 开始嵌套块 (enum { ... }, 匿名 union 等)
            // 注意: 成员函数体的 { 在后面的内联函数检测中处理
            if bytes[i] == b'{' {
                // 检查前面的非空白字符是否是 '=' (如果是, 这是初始化器不是块)
                let mut bk = i;
                while bk > 0
                    && (bytes[bk - 1] == b' '
                        || bytes[bk - 1] == b'\t'
                        || bytes[bk - 1] == b'\n'
                        || bytes[bk - 1] == b'\r')
                {
                    bk -= 1;
                }
                if bk > 0 && bytes[bk - 1] == b'=' {
                    // 这是初始化器 `{...}`, 不是嵌套块
                    // 跳过整个 {...} 并输出
                    let mut depth = 1i32;
                    let mut j = i + 1;
                    while j < len && depth > 0 {
                        if bytes[j] == b'{' {
                            depth += 1;
                        } else if bytes[j] == b'}' {
                            depth -= 1;
                        }
                        j += 1;
                    }
                    result.push_str(&s[i..j]);
                    i = j;
                    continue;
                }
                // 嵌套块 (enum/union 等)
                nested_brace_depth = 1;
                result.push('{');
                i += 1;
                continue;
            }

            // 检测 '}' 结束 struct (只在 nested_brace_depth == 0 时到达这里)
            if bytes[i] == b'}' {
                struct_name_stack.pop();
                result.push('}');
                i += 1;
                // 检查后面是否有 typedef 名称 (如果有, 不添加分号)
                let j = skip_ws(bytes, i);
                if j < len && is_ident_char(bytes[j]) {
                    // 后面有标识符, 可能是 typedef 名称, 不添加分号
                } else if i < len && bytes[i] == b';' {
                    // 后面已经有分号
                    result.push(';');
                    i += 1;
                } else {
                    // 需要添加分号
                    result.push(';');
                }
                // 只有在最外层 struct 结束时才输出收集到的内联成员函数
                // 嵌套 struct 结束时不输出, 等外层 struct 结束时一起输出
                if struct_name_stack.is_empty() {
                    for func in &pending_functions {
                        result.push_str(func);
                    }
                    pending_functions.clear();
                }
                continue;
            }

            // 跳过 mutable 关键字
            if match_word_at(bytes, i, "mutable") {
                i = skip_ws(bytes, i + 7);
                continue;
            }

            // 跳过 static 成员声明 (含初始化)
            if match_word_at(bytes, i, "static") {
                // 跳过到 ';' (可能跨行), 但要跳过嵌套的 {...}
                let mut j = i;
                let mut bd = 0i32;
                while j < len {
                    if bytes[j] == b'"' {
                        j = skip_string_literal(bytes, j);
                        continue;
                    }
                    if bytes[j] == b'{' {
                        bd += 1;
                    } else if bytes[j] == b'}' {
                        if bd == 0 {
                            break;
                        } // struct body closing brace
                        bd -= 1;
                    } else if bytes[j] == b';' && bd == 0 {
                        break;
                    }
                    j += 1;
                }
                if j < len && bytes[j] == b';' {
                    j += 1;
                }
                i = j;
                continue;
            }

            // 检测析构函数声明: ~ClassName(...);
            if bytes[i] == b'~' {
                let mut j = i + 1;
                while j < len && is_ident_char(bytes[j]) {
                    j += 1;
                }
                j = skip_ws(bytes, j);
                if j < len && bytes[j] == b'(' {
                    // 跳过到 ';'
                    while j < len && bytes[j] != b';' {
                        j += 1;
                    }
                    if j < len {
                        j += 1;
                    }
                    i = j;
                    continue;
                }
            }

            // 检测构造函数声明或内联定义: ClassName(...)
            // 或 ClassName(ClassName &), ClassName(ClassName &&)
            if match_word_at(bytes, i, &struct_name) {
                let name_len = struct_name.len();
                let mut j = skip_ws(bytes, i + name_len);
                if j < len && bytes[j] == b'(' {
                    // 这是构造函数声明或定义
                    // 提取参数列表
                    let param_start = j + 1;
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
                    let param_end = j - 1; // position of ')'
                    let params_raw = &s[param_start..param_end];
                    let params_clean = remove_default_args(params_raw);
                    let params_clean = convert_ref_params(&params_clean);
                    let params_clean = add_struct_prefix(&params_clean, &struct_name);

                    // 跳过空白
                    j = skip_ws(bytes, j);
                    // 解析初始化列表 ': member(init), ...'
                    let mut init_list: Vec<(String, String)> = Vec::new();
                    if j < len && bytes[j] == b':' && (j + 1 >= len || bytes[j + 1] != b':') {
                        j += 1;
                        // 解析每个 member(init) 对
                        while j < len && bytes[j] != b'{' && bytes[j] != b';' {
                            j = skip_ws(bytes, j);
                            if j >= len || bytes[j] == b'{' || bytes[j] == b';' {
                                break;
                            }
                            // 读取成员名
                            if is_ident_char(bytes[j]) {
                                let (mname, mname_end) = read_ident(bytes, j);
                                j = skip_ws(bytes, mname_end);
                                // 跳过 '(' ... ')'
                                if j < len && bytes[j] == b'(' {
                                    let mut pd = 1i32;
                                    let init_start = j + 1;
                                    j += 1;
                                    while j < len && pd > 0 {
                                        if bytes[j] == b'(' {
                                            pd += 1;
                                        } else if bytes[j] == b')' {
                                            pd -= 1;
                                        }
                                        j += 1;
                                    }
                                    let init_val = s[init_start..j - 1].trim().to_string();
                                    init_list.push((mname.to_string(), init_val));
                                }
                                j = skip_ws(bytes, j);
                                // 跳过 ','
                                if j < len && bytes[j] == b',' {
                                    j += 1;
                                }
                            } else {
                                j += 1; // skip unknown char
                            }
                        }
                    }
                    // 如果是内联定义 (有 '{')
                    if j < len && bytes[j] == b'{' {
                        // 构建 _ctor 函数
                        let this_param = format!("struct {} *this_", struct_name);
                        let all_params =
                            if params_clean.trim().is_empty() || params_clean.trim() == "void" {
                                this_param
                            } else {
                                format!("{}, {}", this_param, params_clean.trim())
                            };
                        let mut ctor_func = String::new();
                        // Count parameters (including this_) for overloaded ctor naming
                        let param_count = all_params.split(',').count();
                        ctor_func.push_str(&format!(
                            "\nvoid _ctor_{}_{}({}) {{\n",
                            struct_name, param_count, all_params
                        ));
                        // 输出初始化列表: this_->member = init;
                        for (mname, mval) in &init_list {
                            // 替换参数名中的引用为指针访问
                            let mval_fixed = mval.clone();
                            ctor_func.push_str(&format!("  this_->{} = {};\n", mname, mval_fixed));
                        }
                        // 复制构造函数体
                        let body_start = j + 1;
                        let mut brace_depth = 1i32;
                        j += 1;
                        while j < len && brace_depth > 0 {
                            if bytes[j] == b'{' {
                                brace_depth += 1;
                            } else if bytes[j] == b'}' {
                                brace_depth -= 1;
                            }
                            j += 1;
                        }
                        let body = s[body_start..j - 1].trim();
                        if !body.is_empty() {
                            // 替换 body 中的字段名为 this_->field
                            let mut fixed_body = body.to_string();
                            for fname in &field_names {
                                // 替换裸字段名 (前面没有 . 或 ->)
                                let pattern = fname.to_string();
                                let replacement = format!("this_->{}", fname);
                                // 简单替换: 只在单词边界替换
                                fixed_body = replace_word(&fixed_body, &pattern, &replacement);
                            }
                            // 替换 this-> 为 this_->
                            fixed_body = fixed_body.replace("this->", "this_->");
                            // 替换裸成员函数调用: func(args) -> struct_name_func(this_, args)
                            for mfn in &member_fn_names {
                                let pattern = format!("{}(", mfn);
                                let replacement = format!("{}_{}(this_, ", struct_name, mfn);
                                fixed_body = fixed_body.replace(&pattern, &replacement);
                                // 处理无参数情况: func() -> struct_name_func(this_)
                                let empty_pattern = format!("{}_{}(this_, )", struct_name, mfn);
                                let empty_replacement = format!("{}_{}(this_)", struct_name, mfn);
                                fixed_body = fixed_body.replace(&empty_pattern, &empty_replacement);
                            }
                            ctor_func.push_str(&format!("  {}\n", fixed_body.trim()));
                        }
                        ctor_func.push_str("}\n");
                        pending_functions.push(ctor_func);
                    } else if j < len && bytes[j] == b';' {
                        // 只有声明没有 body, 生成空 ctor
                        let this_param = format!("struct {} *this_", struct_name);
                        let all_params =
                            if params_clean.trim().is_empty() || params_clean.trim() == "void" {
                                this_param
                            } else {
                                format!("{}, {}", this_param, params_clean.trim())
                            };
                        let mut ctor_decl = String::new();
                        // Count parameters (including this_) for overloaded ctor naming
                        let param_count = all_params.split(',').count();
                        ctor_decl.push_str(&format!(
                            "\nvoid _ctor_{}_{}({}) {{\n",
                            struct_name, param_count, all_params
                        ));
                        for (mname, mval) in &init_list {
                            ctor_decl.push_str(&format!("  this_->{} = {};\n", mname, mval));
                        }
                        ctor_decl.push_str("}\n");
                        pending_functions.push(ctor_decl);
                        j += 1;
                    }
                    i = j;
                    continue;
                }
            }

            // 检测 = delete 或 = default: 跳过整行
            if bytes[i] == b'=' {
                let mut j = skip_ws(bytes, i + 1);
                if match_word_at(bytes, j, "delete") || match_word_at(bytes, j, "default") {
                    let skip_len = if match_word_at(bytes, j, "delete") { 6 } else { 7 };
                    j = skip_ws(bytes, j + skip_len);
                    if j < len && bytes[j] == b';' {
                        j += 1;
                    }
                    i = j;
                    continue;
                }
            }

            // 检测 operator= 声明: 跳过到 ';'
            if match_word_at(bytes, i, "operator") {
                let mut j = skip_ws(bytes, i + 8);
                if j < len && bytes[j] == b'=' {
                    while j < len && bytes[j] != b';' {
                        j += 1;
                    }
                    if j < len {
                        j += 1;
                    }
                    i = j;
                    continue;
                }
            }

            // 检测内联成员函数定义: `ret_type name(...) { body }`
            // 需要检测 `{` 来判断是否是内联定义
            // 但我们需要区分字段声明和成员函数定义
            // 策略: 如果看到 `ident(...)` 后跟 `{` 或 const/noexcept/override 后跟 `{`,
            // 则是内联成员函数
            // 简化: 检测 `)` 后跟 `{` 的模式

            // 检测模板字段类型: `template_name<...> field_name;`
            // 替换为 `void *field_name;`
            // 先检测标识符后跟 '<'
            if is_ident_char(bytes[i]) && (i == 0 || !is_ident_char(bytes[i - 1])) {
                let (ident, ident_end) = read_ident(bytes, i);
                let j = skip_ws(bytes, ident_end);
                if j < len
                    && bytes[j] == b'<'
                    && ident != "enum"
                    && ident != "return"
                    && ident != "if"
                    && ident != "while"
                    && ident != "for"
                {
                    // 可能是模板类型, 尝试跳过 <...>
                    let mut depth = 1i32;
                    let mut k = j + 1;
                    let mut is_template = true;
                    while k < len && depth > 0 {
                        if bytes[k] == b'<' {
                            depth += 1;
                        } else if bytes[k] == b'>' {
                            depth -= 1;
                        } else if bytes[k] == b';' || bytes[k] == b'}' {
                            is_template = false;
                            break;
                        }
                        k += 1;
                    }
                    if is_template && depth == 0 {
                        // 是模板类型, 检查后面是否有字段名和 ';'
                        k = skip_ws(bytes, k);
                        // 跳过字段名
                        if k < len && is_ident_char(bytes[k]) {
                            let (_, field_end) = read_ident(bytes, k);
                            let mut fe = skip_ws(bytes, field_end);
                            // 跳过可能的数组 [N]
                            while fe < len && bytes[fe] == b'[' {
                                while fe < len && bytes[fe] != b']' {
                                    fe += 1;
                                }
                                if fe < len {
                                    fe += 1;
                                }
                            }
                            if fe < len && bytes[fe] == b';' {
                                // 替换为 void *field_name;
                                let field_name = &s[k..field_end];
                                result.push_str(&format!(" void *{};", field_name));
                                i = fe + 1;
                                continue;
                            }
                        }
                    }
                }
            }

            // 检测内联成员函数: `ret_type name(params) { body }`
            // 或 `ret_type name(params) const { body }`
            // 策略: 找 `)` 后跟 `{` 或 `const`/`noexcept` 后跟 `{`
            // 但我们已经在 struct 体内, 所以任何 `) ... {` 都是成员函数
            // 简化: 扫描到 `;` 或 `{`, 如果是 `{` 则跳过 body

            // 检测是否是成员函数定义 (非构造函数, 已在上面处理)
            // 看当前行是否包含 `(` ... `)` 后跟 `{`
            {
                let mut j = i;
                let mut found_paren = false;
                let mut found_semi = false;
                let mut found_open_brace = false;
                // 扫描到 ';' 或 '{' 或 '}'
                let mut pd = 0i32; // paren depth
                while j < len {
                    if bytes[j] == b'"' {
                        j = skip_string_literal(bytes, j);
                        continue;
                    }
                    if bytes[j] == b'(' {
                        pd += 1;
                        found_paren = true;
                    } else if bytes[j] == b')' {
                        pd -= 1;
                    } else if bytes[j] == b'{' && pd == 0 {
                        found_open_brace = true;
                        break;
                    } else if bytes[j] == b';' && pd == 0 {
                        found_semi = true;
                        break;
                    } else if bytes[j] == b'}' && pd == 0 {
                        break;
                    }
                    j += 1;
                }
                if found_paren && found_open_brace {
                    // 这是内联成员函数定义
                    // 提取返回类型和函数名, 转为独立函数
                    // 先提取 struct_name 和函数签名
                    let sig_start = i;
                    // 找到函数名: 第一个 '(' 前的标识符
                    let mut paren_start = sig_start;
                    {
                        let mut d = 0i32;
                        let mut k = sig_start;
                        let mut found_first_paren = false;
                        while k < j {
                            if bytes[k] == b'(' {
                                if d == 0 && !found_first_paren {
                                    paren_start = k;
                                    found_first_paren = true;
                                }
                                d += 1;
                            } else if bytes[k] == b')' {
                                d -= 1;
                            } else if bytes[k] == b'{' && d == 0 {
                                break;
                            }
                            k += 1;
                        }
                    }
                    // 找函数名: paren_start 前的标识符
                    let mut fn_end = paren_start;
                    while fn_end > sig_start && !is_ident_char(bytes[fn_end - 1]) {
                        fn_end -= 1;
                    }
                    let mut fn_start = fn_end;
                    while fn_start > sig_start && is_ident_char(bytes[fn_start - 1]) {
                        fn_start -= 1;
                    }
                    let fn_name_base = &s[fn_start..fn_end];
                    // 如果是 operator，捕获后面的操作符符号
                    let fn_name = if fn_name_base == "operator" {
                        // 从 fn_end 开始捕获操作符符号
                        let mut op_end = fn_end;
                        while op_end < len
                            && !is_ident_char(bytes[op_end])
                            && bytes[op_end] != b'('
                            && bytes[op_end] != b')'
                        {
                            op_end += 1;
                        }
                        let op_symbols = s[fn_end..op_end].trim();
                        if !op_symbols.is_empty() {
                            format!("operator{}", op_symbols)
                        } else {
                            fn_name_base.to_string()
                        }
                    } else {
                        fn_name_base.to_string()
                    };

                    // Skip C++ control flow keywords that look like function calls
                    // but are not member functions (if, while, for, switch, etc.)
                    if fn_name == "if"
                        || fn_name == "while"
                        || fn_name == "for"
                        || fn_name == "switch"
                        || fn_name == "catch"
                        || fn_name == "return"
                    {
                        // Not a member function, skip to next statement
                        i = if found_open_brace {
                            // Skip the body
                            let mut brace_depth = 1i32;
                            let mut skip_i = j + 1;
                            while skip_i < len && brace_depth > 0 {
                                if bytes[skip_i] == b'{' {
                                    brace_depth += 1;
                                } else if bytes[skip_i] == b'}' {
                                    brace_depth -= 1;
                                }
                                skip_i += 1;
                            }
                            skip_i
                        } else if found_semi {
                            j + 1
                        } else {
                            j
                        };
                        continue;
                    }

                    // 如果是构造函数（函数名等于 struct 名），转换为 _ctor 函数
                    if fn_name == struct_name {
                        // 提取参数列表
                        let paren_end;
                        {
                            let mut d = 1i32;
                            let mut k = paren_start + 1;
                            while k < len && d > 0 {
                                if bytes[k] == b'(' {
                                    d += 1;
                                } else if bytes[k] == b')' {
                                    d -= 1;
                                }
                                k += 1;
                            }
                            paren_end = k - 1;
                        }
                        let params_raw = &s[paren_start + 1..paren_end];
                        let params_clean = remove_default_args(params_raw);
                        let params_clean = convert_ref_params(&params_clean);
                        let params_clean = add_struct_prefix(&params_clean, &struct_name);

                        // 跳过空白
                        let mut skip_j = skip_ws(bytes, paren_end + 1);
                        // 解析初始化列表
                        let mut init_list: Vec<(String, String)> = Vec::new();
                        if skip_j < len
                            && bytes[skip_j] == b':'
                            && (skip_j + 1 >= len || bytes[skip_j + 1] != b':')
                        {
                            skip_j += 1;
                            while skip_j < len && bytes[skip_j] != b'{' && bytes[skip_j] != b';' {
                                skip_j = skip_ws(bytes, skip_j);
                                if skip_j >= len || bytes[skip_j] == b'{' || bytes[skip_j] == b';' {
                                    break;
                                }
                                if is_ident_char(bytes[skip_j]) {
                                    let (mname, mname_end) = read_ident(bytes, skip_j);
                                    skip_j = skip_ws(bytes, mname_end);
                                    if skip_j < len && bytes[skip_j] == b'(' {
                                        let mut pd = 1i32;
                                        let init_start = skip_j + 1;
                                        skip_j += 1;
                                        while skip_j < len && pd > 0 {
                                            if bytes[skip_j] == b'(' {
                                                pd += 1;
                                            } else if bytes[skip_j] == b')' {
                                                pd -= 1;
                                            }
                                            skip_j += 1;
                                        }
                                        let init_val = s[init_start..skip_j - 1].trim().to_string();
                                        init_list.push((mname.to_string(), init_val));
                                    }
                                    skip_j = skip_ws(bytes, skip_j);
                                    if skip_j < len && bytes[skip_j] == b',' {
                                        skip_j += 1;
                                    }
                                } else {
                                    skip_j += 1;
                                }
                            }
                        }
                        // 构建 _ctor 函数
                        let this_param = format!("struct {} *this_", struct_name);
                        let all_params =
                            if params_clean.trim().is_empty() || params_clean.trim() == "void" {
                                this_param
                            } else {
                                format!("{}, {}", this_param, params_clean.trim())
                            };
                        let mut ctor_func = String::new();
                        // Count parameters (including this_) for overloaded ctor naming
                        let param_count = all_params.split(',').count();
                        ctor_func.push_str(&format!(
                            "\nvoid _ctor_{}_{}({}) {{\n",
                            struct_name, param_count, all_params
                        ));
                        for (mname, mval) in &init_list {
                            ctor_func.push_str(&format!("  this_->{} = {};\n", mname, mval));
                        }
                        // 复制构造函数体
                        if skip_j < len && bytes[skip_j] == b'{' {
                            let body_start = skip_j + 1;
                            let mut brace_depth = 1i32;
                            skip_j += 1;
                            while skip_j < len && brace_depth > 0 {
                                if bytes[skip_j] == b'{' {
                                    brace_depth += 1;
                                } else if bytes[skip_j] == b'}' {
                                    brace_depth -= 1;
                                }
                                skip_j += 1;
                            }
                            let body = s[body_start..skip_j - 1].trim();
                            if !body.is_empty() {
                                let mut fixed_body = body.to_string();
                                for fname in &field_names {
                                    fixed_body = replace_word(
                                        &fixed_body,
                                        fname,
                                        &format!("this_->{}", fname),
                                    );
                                }
                                fixed_body = fixed_body.replace("this->", "this_->");
                                ctor_func.push_str(&format!("  {}\n", fixed_body.trim()));
                            }
                        }
                        ctor_func.push_str("}\n");
                        pending_functions.push(ctor_func);
                        i = skip_j;
                        continue;
                    }

                    // 提取参数列表
                    let paren_end;
                    {
                        let mut d = 1i32;
                        let mut k = paren_start + 1;
                        while k < len && d > 0 {
                            if bytes[k] == b'(' {
                                d += 1;
                            } else if bytes[k] == b')' {
                                d -= 1;
                            }
                            k += 1;
                        }
                        paren_end = k - 1; // position of ')'
                    }
                    let params_str = &s[paren_start + 1..paren_end];
                    // 去掉默认参数 (= xxx)
                    let params_clean = remove_default_args(params_str);
                    // 将引用参数 Type& name 转换为 Type* name
                    let params_clean = convert_ref_params(&params_clean);
                    // 为已知 struct 类型添加 struct 前缀
                    let params_clean = add_struct_prefix(&params_clean, &struct_name);

                    // 生成独立函数: ret_type struct_name_fnname(struct struct_name *this, params)
                    let ret_type = s[sig_start..fn_start].trim();
                    // C++ 引用返回 → C 指针返回: `T&` / `T&&` → `T*`
                    // (与形参 convert_ref_params 的引用降级规则一致)
                    let ret_trimmed = ret_type.trim();
                    let (ret_base, ret_is_ref) = if let Some(b) = ret_trimmed.strip_suffix("&&") {
                        (b.trim_end(), true)
                    } else if let Some(b) = ret_trimmed.strip_suffix('&') {
                        (b.trim_end(), true)
                    } else {
                        (ret_trimmed, false)
                    };
                    // 为返回类型添加 struct 前缀
                    let ret_base = if ret_base == struct_name {
                        format!("struct {}", struct_name)
                    } else if ret_base.is_empty() {
                        "void".to_string()
                    } else {
                        ret_base.to_string()
                    };
                    let ret = if ret_is_ref { format!("{}*", ret_base) } else { ret_base };

                    // 构建参数
                    let this_param = format!("struct {} *this_", struct_name);
                    let all_params =
                        if params_clean.trim().is_empty() || params_clean.trim() == "void" {
                            this_param
                        } else {
                            format!("{}, {}", this_param, params_clean.trim())
                        };

                    // 处理 operator 函数名: operator== → operator_eq, operator!= → operator_ne
                    let fn_name_clean = if fn_name.starts_with("operator") {
                        let op = fn_name.trim_start_matches("operator").trim();
                        match op {
                            "==" => "operator_eq".to_string(),
                            "!=" => "operator_ne".to_string(),
                            "<" => "operator_lt".to_string(),
                            ">" => "operator_gt".to_string(),
                            "<=" => "operator_le".to_string(),
                            ">=" => "operator_ge".to_string(),
                            "+" => "operator_add".to_string(),
                            "-" => "operator_sub".to_string(),
                            "*" => "operator_mul".to_string(),
                            "/" => "operator_div".to_string(),
                            "%" => "operator_mod".to_string(),
                            "=" => "operator_assign".to_string(),
                            "[]" => "operator_index".to_string(),
                            "()" => "operator_call".to_string(),
                            "->" => "operator_arrow".to_string(),
                            "," => "operator_comma".to_string(),
                            "->*" => "operator_arrow_star".to_string(),
                            "<<" => "operator_lshift".to_string(),
                            ">>" => "operator_rshift".to_string(),
                            "&" => "operator_and".to_string(),
                            "|" => "operator_or".to_string(),
                            "^" => "operator_xor".to_string(),
                            "~" => "operator_not".to_string(),
                            "!" => "operator_lnot".to_string(),
                            "+=" => "operator_plus_assign".to_string(),
                            "-=" => "operator_minus_assign".to_string(),
                            "*=" => "operator_star_assign".to_string(),
                            "/=" => "operator_slash_assign".to_string(),
                            "%=" => "operator_mod_assign".to_string(),
                            "&=" => "operator_and_assign".to_string(),
                            "|=" => "operator_or_assign".to_string(),
                            "^=" => "operator_xor_assign".to_string(),
                            "<<=" => "operator_lshift_assign".to_string(),
                            ">>=" => "operator_rshift_assign".to_string(),
                            "&&" => "operator_land".to_string(),
                            "||" => "operator_lor".to_string(),
                            _ => {
                                // Sanitize: replace any remaining non-ident chars
                                let sanitized: String = op
                                    .chars()
                                    .map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' })
                                    .collect();
                                format!("operator_{}", sanitized)
                            },
                        }
                    } else {
                        fn_name.to_string()
                    };

                    let mut func_body = String::new();
                    func_body.push_str(&format!(
                        "\n{} {}_{}({}) ",
                        ret, struct_name, fn_name_clean, all_params
                    ));

                    // 跳过 body { ... }
                    j = skip_ws(bytes, paren_end + 1);
                    // 跳过可能的 const/noexcept/override
                    while j < len && bytes[j] != b'{' {
                        j += 1;
                    }
                    if j < len && bytes[j] == b'{' {
                        // 输出 body 但替换 this-> 为 this_->, 替换 param.field 为 param->field
                        func_body.push('{');
                        let mut brace_depth = 1i32;
                        j += 1;
                        // 收集参数名 (用于替换 param.field → param->field)
                        let param_names: Vec<String> = params_clean
                            .split(',')
                            .filter_map(|p| {
                                let parts: Vec<&str> = p.split_whitespace().collect();
                                if parts.len() >= 2 {
                                    // 最后一个部分是参数名 (可能包含 * 或 &)
                                    let name = parts
                                        .last()
                                        .unwrap()
                                        .trim_start_matches('*')
                                        .trim_start_matches('&');
                                    if !name.is_empty() && name != "void" {
                                        return Some(name.to_string());
                                    }
                                }
                                None
                            })
                            .collect();
                        while j < len && brace_depth > 0 {
                            if bytes[j] == b'{' {
                                brace_depth += 1;
                            } else if bytes[j] == b'}' {
                                brace_depth -= 1;
                            }
                            if brace_depth > 0 {
                                // 替换 this-> 为 this_->
                                if match_word_at(bytes, j, "this")
                                    && j + 4 < len
                                    && bytes[j + 4] == b'-'
                                    && j + 5 < len
                                    && bytes[j + 5] == b'>'
                                {
                                    func_body.push_str("this_->");
                                    j += 6;
                                    continue;
                                }
                                // 替换 *this 为 this_ (C++ 引用返回 → C 指针返回)
                                if bytes[j] == b'*' {
                                    let k = skip_ws(bytes, j + 1);
                                    if match_word_at(bytes, k, "this") {
                                        func_body.push_str("this_");
                                        j = k + 4;
                                        continue;
                                    }
                                }
                                // 替换裸成员函数调用: func(args) -> struct_name_func(this_, args)
                                if is_ident_char(bytes[j])
                                    && (j == 0 || !is_ident_char(bytes[j - 1]))
                                {
                                    let (ident, ident_end) = read_ident(bytes, j);
                                    if member_fn_names.contains(&ident.to_string()) {
                                        let sk = skip_ws(bytes, ident_end);
                                        if sk < len && bytes[sk] == b'(' {
                                            let after_paren = skip_ws(bytes, sk + 1);
                                            if after_paren < len && bytes[after_paren] == b')' {
                                                // 无参数: func() -> struct_name_func(this_)
                                                func_body.push_str(&format!(
                                                    "{}_{}(this_)",
                                                    struct_name, ident
                                                ));
                                                j = after_paren + 1; // skip past ')'
                                            } else {
                                                // 有参数: func(args) -> struct_name_func(this_, args)
                                                func_body.push_str(&format!(
                                                    "{}_{}(this_, ",
                                                    struct_name, ident
                                                ));
                                                j = sk + 1; // skip past '('
                                            }
                                            continue;
                                        }
                                    }
                                }
                                // 替换 struct_name 为 struct struct_name (用于返回类型和局部变量)
                                if match_word_at(bytes, j, &struct_name) {
                                    let name_len = struct_name.len();
                                    // 检查前面是否已经有 struct 前缀
                                    let before = if func_body.len() >= 7 {
                                        &func_body[func_body.len() - 7..]
                                    } else {
                                        &func_body
                                    };
                                    if !before.ends_with("struct ") && !before.ends_with("struct\t")
                                    {
                                        func_body.push_str("struct ");
                                    }
                                    func_body.push_str(&struct_name);
                                    j += name_len;
                                    continue;
                                }
                                // 替换 param.field 为 param->field
                                let mut replaced = false;
                                for pname in &param_names {
                                    if match_word_at(bytes, j, pname) {
                                        let plen = pname.len();
                                        let k = skip_ws(bytes, j + plen);
                                        if k < len && bytes[k] == b'.' {
                                            func_body.push_str(pname);
                                            func_body.push_str("->");
                                            j = k + 1;
                                            replaced = true;
                                            break;
                                        }
                                    }
                                }
                                if replaced {
                                    continue;
                                }
                                // 替换裸字段名为 this_->field
                                if is_ident_char(bytes[j])
                                    && (j == 0 || !is_ident_char(bytes[j - 1]))
                                {
                                    let (ident, ident_end) = read_ident(bytes, j);
                                    let is_field = field_names.iter().any(|f| f == &ident);
                                    if is_field {
                                        // 检查前面是否是 . 或 -> (已经是成员访问)
                                        let fb = func_body.as_bytes();
                                        let fl = fb.len();
                                        let after_dot = fl > 0
                                            && (fb[fl - 1] == b'.'
                                                || (fl >= 2
                                                    && fb[fl - 1] == b'>'
                                                    && fb[fl - 2] == b'-'));
                                        if !after_dot {
                                            func_body.push_str("this_->");
                                        }
                                        func_body.push_str(&ident);
                                        j = ident_end;
                                        continue;
                                    }
                                }
                                func_body.push(bytes[j] as char);
                            }
                            j += 1;
                        }
                        func_body.push_str("}\n");
                        // 将函数添加到待输出列表
                        pending_functions.push(func_body);
                    }
                    i = j;
                    continue;
                }
                // 如果只是声明 (found_semi), 删除它 (C struct 体内不能有函数声明)
                if found_paren && found_semi {
                    // 跳过整个声明
                    i = j + 1;
                    continue;
                }
            }

            // 默认: 输出当前字符
            result.push(bytes[i] as char);
            i += 1;
            continue;
        }

        // 不在 struct 体内, 正常输出
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// 为已知 struct 类型添加 struct 前缀: `pair* p` → `struct pair* p`
/// 但不在 struct 定义本身中添加 (即不在 `struct NAME {` 或 `struct NAME;` 中添加)
/// 也不在函数名中添加 (即不在 `NAME(` 中添加)
pub(crate) fn add_struct_prefix(s: &str, struct_name: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let name_len = struct_name.len();
    let mut i = 0;
    while i < len {
        // 检查是否是 struct_name
        if match_word_at(bytes, i, struct_name) {
            // 检查前面是否已经有 struct 前缀 (包括空白字符)
            let mut has_struct_prefix = false;
            let mut k = result.len();
            // 跳过尾部空白
            while k > 0 && (result.as_bytes()[k - 1] as char).is_ascii_whitespace() {
                k -= 1;
            }
            // 检查是否以 "struct" 结尾
            if k >= 6 && &result.as_bytes()[k - 6..k] == b"struct" {
                // 检查 struct 前面是否是标识符边界
                if k == 6 || !is_ident_char(result.as_bytes()[k - 7]) {
                    has_struct_prefix = true;
                }
            }
            // 检查前面是否是 '*' (指针变量名) 或 '&' (引用变量名)
            // 如果是, 则当前名字是变量名, 不是类型名
            let is_var_after_ptr =
                k > 0 && (result.as_bytes()[k - 1] == b'*' || result.as_bytes()[k - 1] == b'&');
            // 检查前面是否是标识符 (类型名), 如果是, 则当前名字是变量名
            // 但需要排除 C/C++ 关键字 (如 const, inline, static 等)
            let mut is_var_after_type =
                k > 0 && is_ident_char(result.as_bytes()[k - 1]) && !has_struct_prefix;
            // 如果前面是标识符, 检查是否是关键字
            if is_var_after_type {
                // 提取前面的标识符
                let mut kend = k;
                while kend > 0 && is_ident_char(result.as_bytes()[kend - 1]) {
                    kend -= 1;
                }
                let prev_ident = &result.as_bytes()[kend..k];
                // 检查是否是 C/C++ 关键字 (这些关键字后面可以跟类型名)
                let is_keyword = prev_ident == b"const"
                    || prev_ident == b"volatile"
                    || prev_ident == b"inline"
                    || prev_ident == b"static"
                    || prev_ident == b"extern"
                    || prev_ident == b"register"
                    || prev_ident == b"auto"
                    || prev_ident == b"typedef"
                    || prev_ident == b"return"
                    || prev_ident == b"sizeof"
                    || prev_ident == b"typeof"
                    || prev_ident == b"__typeof"
                    || prev_ident == b"struct"
                    || prev_ident == b"enum"
                    || prev_ident == b"union";
                // 如果是关键字, 则当前名字是类型名, 不是变量名
                if is_keyword {
                    is_var_after_type = false;
                }
            }
            // 检查前面是否是 '(' 或 ',' (函数参数位置), 如果是, 则当前名字是变量名
            let is_func_arg =
                k > 0 && (result.as_bytes()[k - 1] == b'(' || result.as_bytes()[k - 1] == b',');
            // 检查前面是否是 'return' (构造函数返回), 如果是, 不添加 struct 前缀
            let is_after_return = {
                let rbytes = result.as_bytes();
                let rlen = rbytes.len();
                // 跳过尾部空白
                let mut kend = rlen;
                while kend > 0 && (rbytes[kend - 1] as char).is_ascii_whitespace() {
                    kend -= 1;
                }
                // 检查是否以 "return" 结尾
                kend >= 6
                    && &rbytes[kend - 6..kend] == b"return"
                    && (kend == 6 || !is_ident_char(rbytes[kend - 7]))
            };
            // 检查后面是否是 struct 定义的开始 ({ 或 ; 或 :)
            let j = skip_ws(bytes, i + name_len);
            let is_struct_def =
                j < len && (bytes[j] == b'{' || bytes[j] == b';' || bytes[j] == b':');
            // 检查后面是否是函数调用 (NAME()
            let is_func_call = j < len && bytes[j] == b'(';
            // 检查后面是否是另一个标识符 (类型位置, 如 `pair make_pair`)
            // 如果是, 则当前名字是类型名, 应添加 struct 前缀
            let is_type_before_ident = j < len
                && is_ident_char(bytes[j])
                && bytes[j] != b'{'
                && bytes[j] != b';'
                && bytes[j] != b':';
            // 检查后面是否是 '[' (数组索引), 如果是, 则当前名字是变量名
            let is_array_access = j < len && bytes[j] == b'[';
            // 检查后面是否是 '.' 或 '->' (成员访问的左边), 如果是, 则当前名字是变量名
            let is_member_lhs = j < len && bytes[j] == b'.';
            // 检查后面是否是赋值/比较运算符 (变量在表达式中)
            let is_expr_operator = j < len
                && (bytes[j] == b'='
                    || bytes[j] == b'<'
                    || bytes[j] == b'>'
                    || bytes[j] == b'!'
                    || bytes[j] == b'+'
                    || bytes[j] == b'-'
                    || bytes[j] == b'*'
                    || bytes[j] == b'/'
                    || bytes[j] == b'%'
                    || bytes[j] == b'|'
                    || bytes[j] == b'&'
                    || bytes[j] == b'^'
                    || bytes[j] == b'?'
                    || bytes[j] == b':');
            // 检查前面是否是 '.' 或 '->' (成员访问), 如果是, 则当前名字是成员名
            let is_member_access = k > 0
                && (result.as_bytes()[k - 1] == b'.'
                    || (k >= 2
                        && result.as_bytes()[k - 1] == b'>'
                        && result.as_bytes()[k - 2] == b'-'));
            // 检查前面是否是 '[' (数组索引内部), 如果是, 则当前名字是变量/表达式
            let is_inside_brackets = k > 0 && result.as_bytes()[k - 1] == b'[';

            // 如果后面是标识符 (类型位置), 或者不落在任何"此处不是类型名"
            // 的启发式例外中, 强制添加 struct 前缀
            let is_unqualified_name = !is_func_call
                && !is_var_after_ptr
                && !is_var_after_type
                && !is_func_arg
                && !is_after_return
                && !is_array_access
                && !is_expr_operator
                && !is_member_access
                && !is_member_lhs
                && !is_inside_brackets;
            if !has_struct_prefix && !is_struct_def && (is_type_before_ident || is_unqualified_name)
            {
                result.push_str("struct ");
            }
            result.push_str(struct_name);
            i += name_len;
            continue;
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// 收集所有 struct 名称, 然后在函数参数/返回类型/局部变量中为它们添加 struct 前缀
pub(crate) fn add_struct_prefix_to_types(s: &str) -> String {
    // 第一步: 收集所有 struct 名称
    let mut struct_names: Vec<String> = Vec::new();
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
            let mut j = skip_ws(bytes, i + 6);
            // 跳过可能的属性宏 (如 GTY(...), __attribute__(...), 等)
            // 属性宏格式: IDENT(...) 或 IDENT((...))
            while j < len && is_ident_char(bytes[j]) {
                let (_ident, ident_end) = read_ident(bytes, j);
                let k = skip_ws(bytes, ident_end);
                // 检查后面是否是 '('
                if k < len && bytes[k] == b'(' {
                    // 跳过括号内容
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
                    // 不是属性宏, 这是 struct 名
                    break;
                }
            }
            if j < len && is_ident_char(bytes[j]) {
                let (name, _) = read_ident(bytes, j);
                if !name.is_empty() && !struct_names.contains(&name) {
                    struct_names.push(name);
                }
            }
        }
        i += 1;
    }
    if struct_names.is_empty() {
        return s.to_string();
    }

    // 第二步: 为所有 struct 名称添加 struct 前缀
    let mut result = s.to_string();
    for name in &struct_names {
        result = add_struct_prefix(&result, name);
    }
    result
}

/// Collect member variable names from struct definitions.
/// Returns a map: struct_name -> Vec<member_name>
pub(crate) fn collect_struct_members(s: &str) -> Vec<(String, Vec<String>)> {
    let mut result = Vec::new();
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        // Look for "struct NAME {"
        if match_word_at(bytes, i, "struct") {
            let after_struct = skip_ws(bytes, i + 6);
            if after_struct < len && is_ident_char(bytes[after_struct]) {
                let (sname, sname_end) = read_ident(bytes, after_struct);
                let after_name = skip_ws(bytes, sname_end);
                if after_name < len && bytes[after_name] == b'{' {
                    // Find matching '}'
                    let mut depth = 1i32;
                    let mut j = after_name + 1;
                    let mut members = Vec::new();
                    while j < len && depth > 0 {
                        if bytes[j] == b'{' {
                            depth += 1;
                        } else if bytes[j] == b'}' {
                            depth -= 1;
                        }
                        // Look for member declarations: type m_name;
                        // Simple heuristic: identifier starting with m_
                        else if depth == 1 && is_ident_char(bytes[j]) {
                            let (ident, ident_end) = read_ident(bytes, j);
                            if ident.starts_with("m_") {
                                // Check if followed by whitespace/semicolon/bracket
                                let after = skip_ws(bytes, ident_end);
                                if after < len
                                    && (bytes[after] == b';'
                                        || bytes[after] == b'['
                                        || bytes[after] == b'='
                                        || bytes[after] == b')')
                                    && !members.contains(&ident)
                                {
                                    members.push(ident);
                                }
                            }
                        }
                        j += 1;
                    }
                    if !members.is_empty() {
                        result.push((sname, members));
                    }
                    i = j;
                    continue;
                }
            }
        }
        i += 1;
    }
    result
}

/// M30.3: 修复基本类型被错误添加 struct 前缀的问题.
/// parser 可能将 `signed long int` 等基本类型误加上 `struct` 前缀,
/// 生成无效的 `struct signed long int`. 需要移除 struct 前缀.
pub(crate) fn fix_struct_primitive_types(s: &str) -> String {
    let mut result = s.to_string();
    // 基本类型列表: 这些类型不应该有 struct 前缀
    let primitive_types = [
        "signed long int",
        "unsigned long int",
        "signed long long",
        "unsigned long long",
        "signed int",
        "unsigned int",
        "signed short",
        "unsigned short",
        "signed char",
        "unsigned char",
        "long double",
        "long long",
        "long int",
        "short int",
        "signed",
        "unsigned",
        "long",
        "short",
        "int",
        "char",
        "float",
        "double",
        "void",
        "size_t",
        "ssize_t",
        "ptrdiff_t",
    ];
    for ty in &primitive_types {
        // 替换 `struct <type>` 为 `<type>`
        let pattern = format!("struct {}", ty);
        result = result.replace(&pattern, ty);
    }
    result
}

/// M30.3: 修复模板实例化缺少 struct 关键字的问题.
/// 模板 struct 实例化时可能生成 `identifier {` 而非 `struct identifier {`.
/// 检测这种模式并添加 struct 前缀, 同时添加 typedef 以便无 struct 前缀引用.
pub(crate) fn fix_missing_struct_keywords(s: &str) -> String {
    let c_keywords = [
        "if",
        "else",
        "for",
        "while",
        "do",
        "switch",
        "case",
        "break",
        "continue",
        "return",
        "goto",
        "typedef",
        "struct",
        "union",
        "enum",
        "extern",
        "static",
        "const",
        "volatile",
        "inline",
        "register",
        "void",
        "int",
        "char",
        "float",
        "double",
        "long",
        "short",
        "signed",
        "unsigned",
        "sizeof",
        "typeof",
        "__typeof__",
    ];
    let lines: Vec<&str> = s.lines().collect();
    let mut result = String::with_capacity(s.len());
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        // 检测 "identifier {" 模式 (行首是标识符后跟 '{')
        let is_struct_def = if trimmed.ends_with('{') {
            let before_brace = trimmed.trim_end_matches('{').trim();
            !before_brace.is_empty()
                && !before_brace.contains(' ')
                && !before_brace.contains('*')
                && !before_brace.contains('(')
                && !before_brace.contains(')')
                && !before_brace.contains('=')
                && before_brace
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_')
                && !c_keywords.contains(&before_brace)
                && i + 1 < lines.len()
                && (lines[i + 1].trim() == "};" || lines[i + 1].trim().starts_with("void _ZN"))
        } else {
            false
        };
        if is_struct_def {
            let struct_name = trimmed.trim_end_matches('{').trim();
            result.push_str(&format!("struct {}", lines[i].trim_start()));
            result.push('\n');
            // 跳过 struct 体直到 };
            i += 1;
            while i < lines.len() {
                result.push_str(lines[i]);
                result.push('\n');
                if lines[i].trim() == "};" {
                    // 添加 typedef 以便无 struct 前缀引用
                    result.push_str(&format!("typedef struct {} {};\n", struct_name, struct_name));
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        result.push_str(lines[i]);
        result.push('\n');
        i += 1;
    }
    result
}

/// Add forward declarations for known functions that are used before defined
pub(crate) fn add_forward_declarations(s: &str) -> String {
    let mut result = String::with_capacity(s.len() + 1024);
    let fwd_decls = r#"
/* Forward declarations added by dcc C++ lowering */
extern struct source_range get_range_from_loc (struct line_maps *set, int loc);
struct encoding_rich_location;
struct unpaired_bidi_rich_location;
void _ctor_encoding_rich_location_2(struct encoding_rich_location *this_, struct cpp_reader *pfile);
void _ctor_encoding_rich_location_3(struct encoding_rich_location *this_, struct cpp_reader *pfile, int loc);
void _ctor_unpaired_bidi_rich_location_3(struct unpaired_bidi_rich_location *this_, struct cpp_reader *pfile, int loc);
const char * bidi_to_str (int kind);
int bidi_current_ctx (void);
int bidi_current_ctx_ucn_p (void);
int bidi_current_ctx_loc (void);
void bidi_on_char (int kind, int ucn_p, int loc);
void bidi_on_close (void);
int bidi_vec_count (void);
void rich_loc_set_escape_on_output (struct rich_location *this_, int val);
void rich_loc_add_range (struct rich_location *this_, int loc);
void _ctor_rich_location_5(struct rich_location *this_, struct cpp_reader *pfile, void *lt, int loc);
void _ctor_rich_location_4(struct rich_location *this_, struct cpp_reader *pfile, void *lt);
struct mkdeps;
struct mkdeps *_new_mkdeps(void);
struct cpp_substring_ranges;
void cpp_substring_ranges_add_range(struct cpp_substring_ranges *this_, struct source_range range);
void cpp_substring_ranges_add_n_ranges(struct cpp_substring_ranges *this_, int num, struct cpp_string_location_reader *loc_reader);
struct cpp_string_location_reader;
struct source_range cpp_string_location_reader_get_next(struct cpp_string_location_reader *this_);
struct cpp_display_width_computation;
int cpp_display_width_computation_process_next_codepoint(struct cpp_display_width_computation *this_, struct cpp_decoded_char *out);
int cpp_display_width_computation_advance_display_cols(struct cpp_display_width_computation *this_, int n);
bool cpp_display_width_computation_done(struct cpp_display_width_computation *this_);
/* vec stubs for GCC vector operations */
static inline void vec_truncate(void *vec, unsigned int size) { (void)vec; (void)size; }
static inline unsigned int vec_count(const void *vec) { (void)vec; return 0; }
static inline void vec_push(void *vec, void *elem) { (void)vec; (void)elem; }
/* Stubs for x86 SSE builtins that dcc doesn't support */
static inline int dcc_pmovmskb128(const void *p) { (void)p; return 0; }
static inline void *dcc_pshufb128(void *p, const void *q) { (void)p; (void)q; return p; }
"#;
    if let Some(pos) = s.find('\n') {
        result.push_str(&s[..pos + 1]);
        result.push_str(fwd_decls);
        result.push_str(&s[pos + 1..]);
    } else {
        result.push_str(fwd_decls);
        result.push_str(s);
    }
    result
}

/// M30.3: 修复缺失的指针 typedef.
/// GCC 代码中常见 `typedef struct X *X_p;` 形式的 typedef,
/// parser 可能不发射这些 typedef 但后续代码使用了 X_p 类型.
/// 检测 struct X 存在但 X_p 未定义的情况, 添加 typedef.
pub(crate) fn fix_missing_ptr_typedefs(s: &str) -> String {
    // 常见的 GCC 指针 typedef 模式
    let ptr_typedefs = [
        ("pair", "pair_p"),
        ("type", "type_p"),
        ("options", "options_p"),
        ("outf", "outf_p"),
    ];
    let mut result = String::new();
    let mut inserted = false;
    for line in s.lines() {
        // 在第一个 struct 定义之前插入缺失的 typedef
        if !inserted && line.trim().starts_with("struct ") && line.contains('{') {
            for (struct_name, typedef_name) in &ptr_typedefs {
                // 检查 struct 是否存在
                let struct_pattern = format!("struct {} {{", struct_name);
                let struct_pattern2 = format!("struct {}\n", struct_name);
                if s.contains(&struct_pattern)
                    || s.contains(&struct_pattern2)
                    || s.contains(&format!("struct {} ", struct_name))
                {
                    // 检查 typedef 是否已存在
                    let typedef_pattern =
                        format!("typedef struct {} *{}", struct_name, typedef_name);
                    if !s.contains(&typedef_pattern) {
                        // 检查 typedef_name 是否被使用
                        if s.contains(typedef_name) {
                            result.push_str(&format!(
                                "typedef struct {} *{};\n",
                                struct_name, typedef_name
                            ));
                        }
                    }
                }
            }
            // 也检查 const_type_p
            if s.contains("const_type_p") && !s.contains("typedef const struct type *const_type_p")
            {
                result.push_str("typedef const struct type *const_type_p;\n");
            }
            // flex 生成的 YY_BUFFER_STATE
            if s.contains("YY_BUFFER_STATE")
                && !s.contains("typedef struct yy_buffer_state *YY_BUFFER_STATE")
            {
                result.push_str("typedef struct yy_buffer_state *YY_BUFFER_STATE;\n");
            }
            inserted = true;
        }
        result.push_str(line);
        result.push('\n');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// legacy 把继承摊成「基类当首字段」之前的第一步: 先删掉 C 不认识的基类子句。
    #[test]
    fn base_clause_is_dropped_and_body_is_kept() {
        let out = remove_struct_inheritance("class D : public B { int x; };");
        assert!(out.starts_with("class D"), "{out}");
        assert!(!out.contains("public"), "{out}");
        assert!(out.contains("int x;"), "{out}");
    }

    /// 同一行的 `struct S {` 无继承时必须逐字符原样输出, 字符串字面量里的
    /// `struct A : B` 也不能被当成继承 (整条流水线的幂等性依赖这一点)。
    #[test]
    fn plain_struct_and_string_literals_are_untouched() {
        let src = "struct S { const char* p = \"struct A : B\"; };\n";
        assert_eq!(remove_struct_inheritance(src), src);
    }

    /// 成员表收集是 `m_` 前缀启发式: 只收结构体内一层深、且名字以 `m_` 开头并以
    /// `;`/`[`/`=`/`)` 收尾的标识符。
    #[test]
    fn collect_struct_members_uses_m_prefix_heuristic() {
        let src = "struct Counter {\n  int m_value;\n  char* m_name;\n  int plain;\n};\n";
        let got = collect_struct_members(src);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].0, "Counter");
        assert_eq!(got[0].1, vec!["m_value".to_string(), "m_name".to_string()]);
    }

    /// 嵌套 `{}` 里的名字不属于外层结构体 (depth==1 判据)。
    #[test]
    fn collect_struct_members_ignores_nested_scope() {
        let src = "struct Outer {\n  struct Inner { int m_hidden; } i;\n  int m_visible;\n};\n";
        let got = collect_struct_members(src);
        let outer = got.iter().find(|(n, _)| n == "Outer").expect("Outer");
        assert!(!outer.1.contains(&"m_hidden".to_string()), "{outer:?}");
        assert!(outer.1.contains(&"m_visible".to_string()), "{outer:?}");
    }
}
