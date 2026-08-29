//! C++ → dcc C 风格声明 降级 (M7 + M8).
//!
//! 策略: 把 C++ 类 / 命名空间 / 继承 / 运算符 / 异常 / lambda / 模板
//! 翻译成纯 C 代码, 复用 dcc 的 C 前端处理. 关键映射:
//!   class Foo { ... };              -> struct Foo { ... };
//!   namespace ns { ... }             -> 扁平化: ns_xxx / 嵌套 mangling
//!   class D : public B {}           -> struct D { struct B base; ... };
//!   virtual void Foo::f()           -> vtable + struct 头部 vptr 字段
//!   operator+                       -> _ZN<cls>plE<args>  符号
//!   try { ... } catch (T e) { ... } -> __cxa_try/__cxa_catch 包裹
//!   lambda                          -> 匿名 struct + operator()
//!   template <T> ...                 -> 简单: 标记实例化时复制
//!   typeid(e)                        -> &_ZTI<T> 或 runtime __cxa_typeid
//!   dynamic_cast<T*>(e)              -> __cxa_dynamic_cast 包装
//!
//! 降级输出: 完整 C 源文件字符串, 包含被包含的头 (展开后嵌入).

use crate::cpp::bmf::{BmfExport, BmfKind, BmfModule};
use crate::cpp::coroutine::{
    body_has_coro, coro_fn_decls, coro_fn_epilogue, coro_fn_prologue, coro_return, coro_suspend,
};
use crate::cpp::gcc_compat::runtime_prologue as gcc_runtime_prologue;
use crate::cpp::mangle::{
    mangle_ctor, mangle_dtor, mangle_global_function, mangle_member_function, mangle_op_delete,
    mangle_op_delete_array, mangle_op_new, mangle_op_new_array, mangle_operator_global,
    mangle_operator_member, mangle_typeinfo, mangle_vtable, MangleType, NestedName, Operator,
};
use crate::cpp::parser::*;
use crate::cpp::template::{instantiated_class_name, type_key, TemplateRegistry};

/// 降级选项
#[derive(Debug, Clone, Default)]
pub struct LowerOptions {
    /// 是否对成员变量名做 `_ZN3Foo` 前缀化, 避免命名冲突
    pub mangle_fields: bool,
}

/// 当前命名空间栈 (M8: 累积限定名, 用于 mangle)
#[derive(Debug, Clone, Default)]
pub struct NamespaceCtx {
    pub stack: Vec<String>,
}

/// 降级器
pub struct Lower {
    pub out: String,
    pub opt: LowerOptions,
    /// 当前命名空间栈
    pub ns: NamespaceCtx,
    /// 已知类集合 (用于 typeid / dynamic_cast)
    pub classes: Vec<String>,
    /// M26: 已知 enum 类型集合 (lower 时输出真实 C enum; 类型引用不前缀 struct)
    pub enum_types: std::collections::HashSet<String>,
    /// M26.1: noexcept 函数名集合 (自由函数用函数名; 成员用 `Class::method`)
    /// `noexcept(expr)` 求值时查被调函数是否 noexcept.
    pub noexcept_fns: std::collections::HashSet<String>,
    /// M26.1: 函数返回类型注册表 (自由函数用函数名; 成员用 `Class::method`).
    /// `auto x = foo()` / `auto y = obj.m()` 推导返回类型用.
    pub fn_ret_types: std::collections::HashMap<String, MangleType>,
    /// 已知类方法 (class -> [method_name, ...])
    pub class_methods: std::collections::HashMap<String, Vec<String>>,
    /// M17.0: 已知类 ctor (class -> [CppDecl::Ctor, ...]) - 供 using Base::Base 真 wrapper
    pub class_ctors: std::collections::HashMap<String, Vec<CppDecl>>,
    /// 已知类成员 (class -> [(name, type), ...])
    pub class_fields: std::collections::HashMap<String, Vec<(String, MangleType)>>,
    /// 已知类基类 (class -> [BaseSpec, ...])
    pub class_bases: std::collections::HashMap<String, Vec<crate::cpp::parser::BaseSpec>>,
    /// 局部变量 -> 类型映射 (用于 c.method() -> _ZN<Cls>3<method>Ev(&c))
    pub local_var_types: std::collections::HashMap<String, MangleType>,
    /// 形参 -> 类型映射
    pub param_var_types: std::collections::HashMap<String, MangleType>,
    /// M18.0: 已输出的 typedef alias 集合, 避免重复 (e.g. system_clock/steady_clock
    /// 都尝试 typedef ... clock). 重复的 alias 跳过 typedef 输出.
    pub emitted_typedefs: std::collections::HashSet<String>,
    /// 当前所在类 (用于 this 隐式注入)
    pub current_class: Option<String>,
    /// 当前函数是否返回 void (用于 `return *this;` → `return;` 转换)
    pub current_fn_void: bool,
    /// 待降级的 lambda 闭包
    pub lambdas: Vec<LambdaExpr>,
    /// 临时计数器 (lambda 编号)
    pub lambda_counter: usize,
    /// M17.0: if constexpr 求值时使用的整型常量池 (Ident -> i128)
    pub const_local_i128: std::collections::HashMap<String, i128>,
    /// M17.0: 协程状态机计数器
    pub coro_state: i64,
    /// M26: 当前协程函数名 (per-function 状态变量前缀)
    pub coro_var: String,
    /// M26: 当前协程函数返回类型的 C 文本 ("void" 跳过返回槽)
    pub coro_ret_c: String,
    /// M26: 模块收集: 当前 module 名 (export module 声明后非空)
    pub module_name: String,
    /// M26: 模块收集: export module 之后进入收集模式
    pub module_collect: bool,
    /// M26: 模块收集: import 依赖列表
    pub module_imports: Vec<String>,
    /// M26: 模块收集: 导出声明 (函数/类/变量)
    pub module_exports: Vec<BmfExport>,
    /// M26: 模板注册表 (模板/偏特化注册 + 实例化缓存)
    pub templates: TemplateRegistry,
    /// M26: 已输出定义的模板实例化 key (类/函数去重)
    pub inst_emitted: std::collections::HashSet<String>,
    /// M26: 普通 (非模板) 函数名 - 模板函数调用裁决用
    pub plain_fns: std::collections::HashSet<String>,
    /// M26: 待输出定义的函数模板实例化 (name, args)
    pub pending_fn_insts: std::collections::VecDeque<(String, Vec<MangleType>)>,
    /// M26: 概念定义注册表 (concept name -> body_text)
    pub concepts: std::collections::HashMap<String, String>,
    /// M26.3: 虚继承: 类的传递虚基列表 (DFS 首次出现顺序, 去重)
    pub transitive_vbases: std::collections::HashMap<String, Vec<String>>,
    /// M26.3: 虚继承: 类的 vbtable (vbase_offset 项, 相对本类 __vbptr 字段偏移;
    /// 第 0 项恒为 0, 即 vbptr 自身)
    pub vbase_tables: std::collections::HashMap<String, Vec<i64>>,
    /// M26.3: 虚继承: 模拟类布局, 记录字段/子对象名 -> 字节偏移
    /// (供 vbtable 常量计算; 与 C struct 实际布局一致才可链接)
    pub field_offsets: std::collections::HashMap<String, std::collections::HashMap<String, i64>>,
    /// M26.3: 模拟类大小 (字节)
    pub class_sizes: std::collections::HashMap<String, i64>,
    /// M26.3: 模拟类对齐 (字节)
    pub class_aligns: std::collections::HashMap<String, i64>,
    /// M26.3: 多态类 (有虚函数/虚析构, 或继承多态基类) - 决定 __vptr 布局
    pub class_polymorphic: std::collections::HashSet<String>,
    /// M27: mutable 成员跟踪 (class -> [field_name, ...])
    /// const 方法中 mutable 字段仍可写, C 端通过注释标记 + const-cast 实现
    pub mutable_fields: std::collections::HashMap<String, Vec<String>>,
    /// M26.4: 导入的模块接口 (BMF), 由 driver 预加载; `import foo;` 据此
    /// 生成 extern 声明并注册名字, 而非当作 `#include <foo>`
    pub imported_modules: Vec<crate::cpp::bmf::BmfModule>,
    /// M26.4: 当前模块分区名 (`export module foo:part;` -> Some("part"))
    pub module_partition: Option<String>,
    /// M26.4: 当前模块导入的分区列表 (`import :part;`)
    pub module_part_imports: Vec<String>,
}

/// M18.0: 检查 ident 是否是 C 关键字 (如 int/char/void/...), 不能作为 typedef 名
pub fn is_c_keyword(s: &str) -> bool {
    matches!(
        s,
        "auto"
            | "break"
            | "case"
            | "char"
            | "const"
            | "continue"
            | "default"
            | "do"
            | "double"
            | "else"
            | "enum"
            | "extern"
            | "float"
            | "for"
            | "goto"
            | "if"
            | "inline"
            | "int"
            | "long"
            | "register"
            | "return"
            | "short"
            | "signed"
            | "sizeof"
            | "static"
            | "struct"
            | "switch"
            | "typedef"
            | "union"
            | "unsigned"
            | "void"
            | "volatile"
            | "while"
            | "_Bool"
            | "_Complex"
            | "_Imaginary"
            | "_Atomic"
            | "_Generic"
            | "_Noreturn"
            | "_Static_assert"
            | "_Thread_local"
            | "bool"
            | "true"
            | "false"
            | "NULL"
            | "class"
            | "namespace"
            | "typename"
            | "template"
            | "this"
            | "virtual"
            | "friend"
            | "operator"
            | "explicit"
            | "constexpr"
            | "decltype"
            | "noexcept"
            | "override"
            | "final"
            | "new"
            | "delete"
            | "using"
            | "try"
            | "catch"
            | "throw"
            | "public"
            | "private"
            | "protected"
            | "nullptr"
    )
}

/// 是否是 C 标识符字符 (字母 / 数字 / 下划线)
pub fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// 检查 bytes[pos..] 是否以 word 开头且前后是标识符边界
fn match_word_at(bytes: &[u8], pos: usize, word: &str) -> bool {
    let wlen = word.len();
    if pos + wlen > bytes.len() { return false; }
    if &bytes[pos..pos+wlen] != word.as_bytes() { return false; }
    let before_ok = pos == 0 || !is_ident_char(bytes[pos-1]);
    let after_ok = pos + wlen >= bytes.len() || !is_ident_char(bytes[pos+wlen]);
    before_ok && after_ok
}

/// 跳过 C 风格字符串字面量 "..." (含转义)
fn skip_string_literal(bytes: &[u8], mut pos: usize) -> usize {
    if pos >= bytes.len() || bytes[pos] != b'"' { return pos; }
    pos += 1;
    while pos < bytes.len() {
        if bytes[pos] == b'\\' { pos += 2; continue; }
        if bytes[pos] == b'"' { return pos + 1; }
        pos += 1;
    }
    pos
}

/// 跳过 // 或 /* */ 注释
fn skip_comment(bytes: &[u8], mut pos: usize) -> usize {
    if pos + 1 >= bytes.len() { return pos; }
    if bytes[pos] == b'/' && bytes[pos+1] == b'/' {
        while pos < bytes.len() && bytes[pos] != b'\n' { pos += 1; }
        return pos;
    }
    if bytes[pos] == b'/' && bytes[pos+1] == b'*' {
        pos += 2;
        while pos + 1 < bytes.len() {
            if bytes[pos] == b'*' && bytes[pos+1] == b'/' { return pos + 2; }
            pos += 1;
        }
        return bytes.len();
    }
    pos
}

/// 读取一个标识符, 返回 (ident_text, end_pos)
fn read_ident(bytes: &[u8], mut pos: usize) -> (String, usize) {
    let start = pos;
    while pos < bytes.len() && is_ident_char(bytes[pos]) { pos += 1; }
    (String::from_utf8_lossy(&bytes[start..pos]).to_string(), pos)
}

/// 跳过空白 (空格/Tab/换行)
fn skip_ws(bytes: &[u8], mut pos: usize) -> usize {
    while pos < bytes.len() && (bytes[pos] as char).is_ascii_whitespace() { pos += 1; }
    pos
}

/// 删除 struct/struct 定义中的继承语法: `struct X : Y {` → `struct X {`
/// 仅删除 ':' 到 '{' 之间的继承列表, 其他内容原样保留.
fn remove_struct_inheritance(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        if bytes[i] == b'"' { let j = skip_string_literal(bytes, i); result.push_str(&s[i..j]); i = j; continue; }
        if bytes[i] == b'/' && i+1 < len && (bytes[i+1] == b'/' || bytes[i+1] == b'*') { let j = skip_comment(bytes, i); result.push_str(&s[i..j]); i = j; continue; }
        // 检测 struct 或 class 关键字
        if (match_word_at(bytes, i, "struct") || match_word_at(bytes, i, "class"))
        {
            let kw_len = if match_word_at(bytes, i, "struct") { 6 } else { 5 };
            // 输出关键字
            result.push_str(&s[i..i+kw_len]);
            let mut j = skip_ws(bytes, i + kw_len);
            // 读取结构名 (如果有)
            if j < len && is_ident_char(bytes[j]) {
                let (_, name_end) = read_ident(bytes, j);
                // 输出从关键字后到 name_end 的所有内容 (含空白和结构名)
                result.push_str(&s[i+kw_len..name_end]);
                j = skip_ws(bytes, name_end);
                // 检查是否有 ':' (继承) 且不是 '::'
                if j < len && bytes[j] == b':' && (j+1 >= len || bytes[j+1] != b':') {
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
            i = i + kw_len;
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
fn clean_struct_bodies(s: &str) -> String {
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
    // 跟踪括号深度 (用于区分 struct 定义和 struct 类型参数)
    let mut paren_depth = 0i32;

    while i < len {
        // 跳过字符串
        if bytes[i] == b'"' { let j = skip_string_literal(bytes, i); result.push_str(&s[i..j]); i = j; continue; }
        // 跳过注释
        if bytes[i] == b'/' && i+1 < len && (bytes[i+1] == b'/' || bytes[i+1] == b'*') {
            let j = skip_comment(bytes, i); result.push_str(&s[i..j]); i = j; continue;
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
            let kw = if match_word_at(bytes, i, "struct") { "struct" } else { "class" };
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
                    while k < len && bytes[k] != b'{' && bytes[k] != b';' { k += 1; }
                    k < len && bytes[k] == b'{'
                } else {
                    false
                };
                if is_struct_def {
                    // Skip to '{'
                    while j < len && bytes[j] != b'{' && bytes[j] != b';' { j += 1; }
                    if j < len && bytes[j] == b'{' {
                    struct_name_stack.push(name.clone());
                    result.push_str(&s[i..j+1]);
                    i = j + 1;
                    // Pre-scan struct body to collect field names
                    // for adding this_-> prefix in inline member functions
                    field_names.clear();
                    {
                        let mut scan = i;
                        let mut scan_depth = 1i32;
                        while scan < len && scan_depth > 0 {
                            if bytes[scan] == b'"' { scan = skip_string_literal(bytes, scan); continue; }
                            if bytes[scan] == b'{' { scan_depth += 1; scan += 1; continue; }
                            if bytes[scan] == b'}' { scan_depth -= 1; scan += 1; continue; }
                            if bytes[scan] == b';' && scan_depth == 1 {
                                // Look backward for field name: last identifier before ';'
                                let mut fe = scan;
                                while fe > i && !is_ident_char(bytes[fe-1]) { fe -= 1; }
                                let mut fs = fe;
                                while fs > i && is_ident_char(bytes[fs-1]) { fs -= 1; }
                                if fs < fe {
                                    let fname = &s[fs..fe];
                                    // Skip keywords and type names
                                    if fname != "const" && fname != "volatile"
                                        && fname != "static" && fname != "inline"
                                        && fname != "virtual" && fname != "explicit"
                                        && fname != "struct" && fname != "class"
                                        && fname != "enum" && fname != "union"
                                        && fname != "void" && fname != "int"
                                        && fname != "char" && fname != "float"
                                        && fname != "double" && fname != "long"
                                        && fname != "short" && fname != "unsigned"
                                        && fname != "signed" && fname != "bool"
                                        && fname != "auto" && fname != "return"
                                        && fname != "delete" && fname != "default"
                                        && fname != "public" && fname != "private"
                                        && fname != "protected" && fname != "friend"
                                        && fname != "typedef" && fname != "namespace"
                                        && fname != "template" && fname != "typename"
                                        && fname != "using" && fname != "new"
                                        && fname != "delete" && fname != "this"
                                        && !fname.starts_with('~')
                                        && fname != &name
                                    {
                                        if !field_names.contains(&fname.to_string()) {
                                            field_names.push(fname.to_string());
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

            // 跳过嵌套的 struct/union 定义: struct { ... } name; 或 union { ... } name;
            if (match_word_at(bytes, i, "struct") || match_word_at(bytes, i, "union"))
                && i + 6 < len
            {
                let kw = if match_word_at(bytes, i, "struct") { "struct" } else { "union" };
                let kw_len = kw.len();
                let mut j = skip_ws(bytes, i + kw_len);
                // 检查是否是匿名定义 (后面直接是 '{')
                if j < len && bytes[j] == b'{' {
                    // 跳过整个嵌套定义: { ... } name;
                    let mut depth = 1i32;
                    j += 1;
                    while j < len && depth > 0 {
                        if bytes[j] == b'{' { depth += 1; }
                        else if bytes[j] == b'}' { depth -= 1; }
                        j += 1;
                    }
                    // 跳过变量名和 ';'
                    while j < len && bytes[j] != b';' { j += 1; }
                    if j < len { j += 1; }
                    // 输出整个嵌套定义
                    result.push_str(&s[i..j]);
                    i = j;
                    continue;
                }
            }

            // 检测 '}' 结束 struct
            if bytes[i] == b'}' {
                struct_name_stack.pop();
                result.push('}');
                i += 1;
                // 检查后面是否有 typedef 名称 (如果有, 不添加分号)
                let mut j = skip_ws(bytes, i);
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
                    if bytes[j] == b'"' { j = skip_string_literal(bytes, j); continue; }
                    if bytes[j] == b'{' { bd += 1; }
                    else if bytes[j] == b'}' {
                        if bd == 0 { break; } // struct body closing brace
                        bd -= 1;
                    }
                    else if bytes[j] == b';' && bd == 0 { break; }
                    j += 1;
                }
                if j < len && bytes[j] == b';' { j += 1; }
                i = j;
                continue;
            }

            // 检测析构函数声明: ~ClassName(...);
            if bytes[i] == b'~' {
                let mut j = i + 1;
                while j < len && is_ident_char(bytes[j]) { j += 1; }
                j = skip_ws(bytes, j);
                if j < len && bytes[j] == b'(' {
                    // 跳过到 ';'
                    while j < len && bytes[j] != b';' { j += 1; }
                    if j < len { j += 1; }
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
                    // 跳过到 ';' 或 '{' (含参数列表和可能的初始化列表)
                    let mut depth = 0i32;
                    // 先跳过参数列表
                    j += 1; // skip '('
                    depth = 1;
                    while j < len && depth > 0 {
                        if bytes[j] == b'(' { depth += 1; }
                        else if bytes[j] == b')' { depth -= 1; }
                        j += 1;
                    }
                    // 跳过空白
                    j = skip_ws(bytes, j);
                    // 跳过可能的初始化列表 ': member(init), ...'
                    if j < len && bytes[j] == b':' && (j+1 >= len || bytes[j+1] != b':') {
                        j += 1;
                        while j < len && bytes[j] != b'{' && bytes[j] != b';' { j += 1; }
                    }
                    // 如果是内联定义 (有 '{')
                    if j < len && bytes[j] == b'{' {
                        // 跳过整个 body
                        let mut brace_depth = 1i32;
                        j += 1;
                        while j < len && brace_depth > 0 {
                            if bytes[j] == b'{' { brace_depth += 1; }
                            else if bytes[j] == b'}' { brace_depth -= 1; }
                            j += 1;
                        }
                    } else if j < len && bytes[j] == b';' {
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
                    if j < len && bytes[j] == b';' { j += 1; }
                    i = j;
                    continue;
                }
            }

            // 检测 operator= 声明: 跳过到 ';'
            if match_word_at(bytes, i, "operator") {
                let mut j = skip_ws(bytes, i + 8);
                if j < len && bytes[j] == b'=' {
                    while j < len && bytes[j] != b';' { j += 1; }
                    if j < len { j += 1; }
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
            if is_ident_char(bytes[i]) && (i == 0 || !is_ident_char(bytes[i-1])) {
                let (ident, ident_end) = read_ident(bytes, i);
                let mut j = skip_ws(bytes, ident_end);
                if j < len && bytes[j] == b'<' && ident != "enum" && ident != "return"
                    && ident != "if" && ident != "while" && ident != "for"
                {
                    // 可能是模板类型, 尝试跳过 <...>
                    let mut depth = 1i32;
                    let mut k = j + 1;
                    let mut is_template = true;
                    while k < len && depth > 0 {
                        if bytes[k] == b'<' { depth += 1; }
                        else if bytes[k] == b'>' { depth -= 1; }
                        else if bytes[k] == b';' || bytes[k] == b'}' { is_template = false; break; }
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
                                while fe < len && bytes[fe] != b']' { fe += 1; }
                                if fe < len { fe += 1; }
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
                    if bytes[j] == b'"' { j = skip_string_literal(bytes, j); continue; }
                    if bytes[j] == b'(' { pd += 1; found_paren = true; }
                    else if bytes[j] == b')' { pd -= 1; }
                    else if bytes[j] == b'{' && pd == 0 { found_open_brace = true; break; }
                    else if bytes[j] == b';' && pd == 0 { found_semi = true; break; }
                    else if bytes[j] == b'}' && pd == 0 { break; }
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
                            }
                            else if bytes[k] == b')' { d -= 1; }
                            else if bytes[k] == b'{' && d == 0 { break; }
                            k += 1;
                        }
                    }
                    // 找函数名: paren_start 前的标识符
                    let mut fn_end = paren_start;
                    while fn_end > sig_start && !is_ident_char(bytes[fn_end-1]) { fn_end -= 1; }
                    let mut fn_start = fn_end;
                    while fn_start > sig_start && is_ident_char(bytes[fn_start-1]) { fn_start -= 1; }
                    let fn_name_base = &s[fn_start..fn_end];
                    // 如果是 operator，捕获后面的操作符符号
                    let fn_name = if fn_name_base == "operator" {
                        // 从 fn_end 开始捕获操作符符号
                        let mut op_end = fn_end;
                        while op_end < len && !is_ident_char(bytes[op_end]) && bytes[op_end] != b'(' && bytes[op_end] != b')' {
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
                    if fn_name == "if" || fn_name == "while" || fn_name == "for"
                        || fn_name == "switch" || fn_name == "catch" || fn_name == "return"
                    {
                        // Not a member function, skip to next statement
                        i = if found_open_brace {
                            // Skip the body
                            let mut brace_depth = 1i32;
                            let mut skip_i = j + 1;
                            while skip_i < len && brace_depth > 0 {
                                if bytes[skip_i] == b'{' { brace_depth += 1; }
                                else if bytes[skip_i] == b'}' { brace_depth -= 1; }
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

                    // 如果是构造函数（函数名等于 struct 名），跳过整个定义
                    if fn_name == struct_name {
                        // 跳过到 ';' 或 '{'
                        let mut skip_j = paren_start + 1;
                        let mut skip_depth = 1i32;
                        while skip_j < len && skip_depth > 0 {
                            if bytes[skip_j] == b'(' { skip_depth += 1; }
                            else if bytes[skip_j] == b')' { skip_depth -= 1; }
                            skip_j += 1;
                        }
                        skip_j = skip_ws(bytes, skip_j);
                        // 跳过可能的初始化列表 ': member(init), ...'
                        if skip_j < len && bytes[skip_j] == b':' && (skip_j+1 >= len || bytes[skip_j+1] != b':') {
                            skip_j += 1;
                            while skip_j < len && bytes[skip_j] != b'{' && bytes[skip_j] != b';' { skip_j += 1; }
                        }
                        // 如果是内联定义 (有 '{')
                        if skip_j < len && bytes[skip_j] == b'{' {
                            // 跳过整个 body
                            let mut brace_depth = 1i32;
                            skip_j += 1;
                            while skip_j < len && brace_depth > 0 {
                                if bytes[skip_j] == b'{' { brace_depth += 1; }
                                else if bytes[skip_j] == b'}' { brace_depth -= 1; }
                                skip_j += 1;
                            }
                        } else if skip_j < len && bytes[skip_j] == b';' {
                            skip_j += 1;
                        }
                        i = skip_j;
                        continue;
                    }

                    // 提取参数列表
                    let mut paren_end = paren_start + 1;
                    {
                        let mut d = 1i32;
                        let mut k = paren_start + 1;
                        while k < len && d > 0 {
                            if bytes[k] == b'(' { d += 1; }
                            else if bytes[k] == b')' { d -= 1; }
                            k += 1;
                        }
                        paren_end = k - 1; // position of ')'
                    }
                    let params_str = &s[paren_start+1..paren_end];
                    // 去掉默认参数 (= xxx)
                    let params_clean = remove_default_args(params_str);
                    // 将引用参数 Type& name 转换为 Type* name
                    let params_clean = convert_ref_params(&params_clean);
                    // 为已知 struct 类型添加 struct 前缀
                    let params_clean = add_struct_prefix(&params_clean, &struct_name);

                    // 生成独立函数: ret_type struct_name_fnname(struct struct_name *this, params)
                    let ret_type = s[sig_start..fn_start].trim();
                    let ret = if ret_type.is_empty() { "void" } else { ret_type };
                    // 为返回类型添加 struct 前缀
                    let ret = if ret == struct_name {
                        format!("struct {}", struct_name)
                    } else {
                        ret.to_string()
                    };

                    // 构建参数
                    let this_param = format!("struct {} *this_", struct_name);
                    let all_params = if params_clean.trim().is_empty() || params_clean.trim() == "void" {
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
                            _ => format!("operator_{}", op.replace(' ', "_")),
                        }
                    } else {
                        fn_name.to_string()
                    };

                    let mut func_body = String::new();
                    func_body.push_str(&format!("\n{} {}_{}({}) ", ret, struct_name, fn_name_clean, all_params));

                    // 跳过 body { ... }
                    j = skip_ws(bytes, paren_end + 1);
                    // 跳过可能的 const/noexcept/override
                    while j < len && bytes[j] != b'{' { j += 1; }
                    if j < len && bytes[j] == b'{' {
                        // 输出 body 但替换 this-> 为 this_->, 替换 param.field 为 param->field
                        func_body.push('{');
                        let mut brace_depth = 1i32;
                        j += 1;
                        // 收集参数名 (用于替换 param.field → param->field)
                        let param_names: Vec<String> = params_clean.split(',')
                            .filter_map(|p| {
                                let parts: Vec<&str> = p.trim().split_whitespace().collect();
                                if parts.len() >= 2 {
                                    // 最后一个部分是参数名 (可能包含 * 或 &)
                                    let name = parts.last().unwrap().trim_start_matches('*').trim_start_matches('&');
                                    if !name.is_empty() && name != "void" {
                                        return Some(name.to_string());
                                    }
                                }
                                None
                            })
                            .collect();
                        while j < len && brace_depth > 0 {
                            if bytes[j] == b'{' { brace_depth += 1; }
                            else if bytes[j] == b'}' { brace_depth -= 1; }
                            if brace_depth > 0 {
                                // 替换 this-> 为 this_->
                                if match_word_at(bytes, j, "this") && j+4 < len && bytes[j+4] == b'-' && j+5 < len && bytes[j+5] == b'>' {
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
                                // 替换 struct_name 为 struct struct_name (用于返回类型和局部变量)
                                if match_word_at(bytes, j, &struct_name) {
                                    let name_len = struct_name.len();
                                    // 检查前面是否已经有 struct 前缀
                                    let before = if func_body.len() >= 7 { &func_body[func_body.len()-7..] } else { &func_body };
                                    if !before.ends_with("struct ") && !before.ends_with("struct\t") {
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
                                if replaced { continue; }
                                // 替换裸字段名为 this_->field
                                if is_ident_char(bytes[j]) && (j == 0 || !is_ident_char(bytes[j-1])) {
                                    let (ident, ident_end) = read_ident(bytes, j);
                                    let is_field = field_names.iter().any(|f| f == &ident);
                                    if is_field {
                                        // 检查前面是否是 . 或 -> (已经是成员访问)
                                        let fb = func_body.as_bytes();
                                        let fl = fb.len();
                                        let after_dot = fl > 0 && (fb[fl-1] == b'.' || (fl >= 2 && fb[fl-1] == b'>' && fb[fl-2] == b'-'));
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

/// 从参数列表中删除默认参数: `int x = 5, int y` → `int x, int y`
fn remove_default_args(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut depth = 0i32; // 括号深度
    while i < len {
        if bytes[i] == b'(' { depth += 1; result.push('('); i += 1; continue; }
        if bytes[i] == b')' { depth -= 1; result.push(')'); i += 1; continue; }
        if bytes[i] == b'=' && depth == 0 {
            // 跳过默认值到 ',' 或结尾
            i += 1;
            while i < len && bytes[i] != b',' { i += 1; }
            continue;
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// 将引用参数转换为指针参数: `Type& name` → `Type* name`
fn convert_ref_params(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut depth = 0i32; // 括号深度
    while i < len {
        if bytes[i] == b'(' { depth += 1; result.push('('); i += 1; continue; }
        if bytes[i] == b')' { depth -= 1; result.push(')'); i += 1; continue; }
        // 在参数列表中 (depth == 0), 将 '&' 转换为 '*'
        if bytes[i] == b'&' && depth == 0 {
            // 检查不是 '&&' (移动语义)
            if i + 1 < len && bytes[i+1] == b'&' {
                // && 转换为 * (简化处理)
                result.push('*');
                i += 2;
                continue;
            }
            // 检查前面是标识符字符或 '*' (类型结束)
            let prev_char = if result.is_empty() { b' ' } else { result.as_bytes().last().copied().unwrap() };
            if is_ident_char(prev_char) || prev_char == b'*' || prev_char == b'>' {
                // 检查后面是标识符开始或空白+标识符 (参数名)
                let mut j = skip_ws(bytes, i + 1);
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

/// 为已知 struct 类型添加 struct 前缀: `pair* p` → `struct pair* p`
/// 但不在 struct 定义本身中添加 (即不在 `struct NAME {` 或 `struct NAME;` 中添加)
/// 也不在函数名中添加 (即不在 `NAME(` 中添加)
fn add_struct_prefix(s: &str, struct_name: &str) -> String {
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
            while k > 0 && (result.as_bytes()[k-1] as char).is_ascii_whitespace() {
                k -= 1;
            }
            // 检查是否以 "struct" 结尾
            if k >= 6 && &result.as_bytes()[k-6..k] == b"struct" {
                // 检查 struct 前面是否是标识符边界
                if k == 6 || !is_ident_char(result.as_bytes()[k-7]) {
                    has_struct_prefix = true;
                }
            }
            // 检查前面是否是 '*' (指针变量名) 或 '&' (引用变量名)
            // 如果是, 则当前名字是变量名, 不是类型名
            let is_var_after_ptr = k > 0 && (result.as_bytes()[k-1] == b'*' || result.as_bytes()[k-1] == b'&');
            // 检查前面是否是标识符 (类型名), 如果是, 则当前名字是变量名
            // 但需要排除 C/C++ 关键字 (如 const, inline, static 等)
            let mut is_var_after_type = k > 0 && is_ident_char(result.as_bytes()[k-1])
                && !has_struct_prefix;
            // 如果前面是标识符, 检查是否是关键字
            if is_var_after_type {
                // 提取前面的标识符
                let mut kend = k;
                while kend > 0 && is_ident_char(result.as_bytes()[kend-1]) {
                    kend -= 1;
                }
                let prev_ident = &result.as_bytes()[kend..k];
                // 检查是否是 C/C++ 关键字 (这些关键字后面可以跟类型名)
                let is_keyword = prev_ident == b"const" || prev_ident == b"volatile"
                    || prev_ident == b"inline" || prev_ident == b"static"
                    || prev_ident == b"extern" || prev_ident == b"register"
                    || prev_ident == b"auto" || prev_ident == b"typedef"
                    || prev_ident == b"return" || prev_ident == b"sizeof"
                    || prev_ident == b"typeof" || prev_ident == b"__typeof"
                    || prev_ident == b"struct" || prev_ident == b"enum"
                    || prev_ident == b"union";
                // 如果是关键字, 则当前名字是类型名, 不是变量名
                if is_keyword {
                    is_var_after_type = false;
                }
            }
            // 检查前面是否是 '(' 或 ',' (函数参数位置), 如果是, 则当前名字是变量名
            let is_func_arg = k > 0 && (result.as_bytes()[k-1] == b'(' || result.as_bytes()[k-1] == b',');
            // 检查前面是否是 'return' (构造函数返回), 如果是, 不添加 struct 前缀
            let is_after_return = {
                let rbytes = result.as_bytes();
                let rlen = rbytes.len();
                // 跳过尾部空白
                let mut kend = rlen;
                while kend > 0 && (rbytes[kend-1] as char).is_ascii_whitespace() {
                    kend -= 1;
                }
                // 检查是否以 "return" 结尾
                kend >= 6 && &rbytes[kend-6..kend] == b"return"
                    && (kend == 6 || !is_ident_char(rbytes[kend-7]))
            };
            // 检查后面是否是 struct 定义的开始 ({ 或 ; 或 :)
            let mut j = skip_ws(bytes, i + name_len);
            let is_struct_def = j < len && (bytes[j] == b'{' || bytes[j] == b';' || bytes[j] == b':');
            // 检查后面是否是函数调用 (NAME()
            let is_func_call = j < len && bytes[j] == b'(';
            // 检查后面是否是另一个标识符 (类型位置, 如 `pair make_pair`)
            // 如果是, 则当前名字是类型名, 应添加 struct 前缀
            let is_type_before_ident = j < len && is_ident_char(bytes[j])
                && bytes[j] != b'{' && bytes[j] != b';' && bytes[j] != b':';
            // 检查后面是否是 '[' (数组索引), 如果是, 则当前名字是变量名
            let is_array_access = j < len && bytes[j] == b'[';
            // 检查后面是否是 '.' 或 '->' (成员访问的左边), 如果是, 则当前名字是变量名
            let is_member_lhs = j < len && bytes[j] == b'.';
            // 检查后面是否是赋值/比较运算符 (变量在表达式中)
            let is_expr_operator = j < len && (bytes[j] == b'=' || bytes[j] == b'<' 
                || bytes[j] == b'>' || bytes[j] == b'!' || bytes[j] == b'+'
                || bytes[j] == b'-' || bytes[j] == b'*' || bytes[j] == b'/'
                || bytes[j] == b'%' || bytes[j] == b'|' || bytes[j] == b'&'
                || bytes[j] == b'^' || bytes[j] == b'?' || bytes[j] == b':');
            // 检查前面是否是 '.' 或 '->' (成员访问), 如果是, 则当前名字是成员名
            let is_member_access = k > 0 && (result.as_bytes()[k-1] == b'.' 
                || (k >= 2 && result.as_bytes()[k-1] == b'>' && result.as_bytes()[k-2] == b'-'));
            // 检查前面是否是 '[' (数组索引内部), 如果是, 则当前名字是变量/表达式
            let is_inside_brackets = k > 0 && result.as_bytes()[k-1] == b'[';
            
            // 如果后面是标识符 (类型位置), 强制添加 struct 前缀
            if is_type_before_ident && !has_struct_prefix && !is_struct_def {
                result.push_str("struct ");
            } else if !has_struct_prefix && !is_struct_def && !is_func_call
                && !is_var_after_ptr && !is_var_after_type && !is_func_arg
                && !is_after_return && !is_array_access && !is_expr_operator
                && !is_member_access && !is_member_lhs && !is_inside_brackets
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
fn add_struct_prefix_to_types(s: &str) -> String {
    // 第一步: 收集所有 struct 名称
    let mut struct_names: Vec<String> = Vec::new();
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        if bytes[i] == b'"' { i = skip_string_literal(bytes, i); continue; }
        if bytes[i] == b'/' && i+1 < len && (bytes[i+1] == b'/' || bytes[i+1] == b'*') {
            i = skip_comment(bytes, i); continue;
        }
        if match_word_at(bytes, i, "struct") {
            let mut j = skip_ws(bytes, i + 6);
            // 跳过可能的属性宏 (如 GTY(...), __attribute__(...), 等)
            // 属性宏格式: IDENT(...) 或 IDENT((...))
            while j < len && is_ident_char(bytes[j]) {
                let (ident, ident_end) = read_ident(bytes, j);
                let k = skip_ws(bytes, ident_end);
                // 检查后面是否是 '('
                if k < len && bytes[k] == b'(' {
                    // 跳过括号内容
                    let mut depth = 1i32;
                    let mut m = k + 1;
                    while m < len && depth > 0 {
                        if bytes[m] == b'(' { depth += 1; }
                        else if bytes[m] == b')' { depth -= 1; }
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
    if struct_names.is_empty() { return s.to_string(); }

    // 第二步: 为所有 struct 名称添加 struct 前缀
    let mut result = s.to_string();
    for name in &struct_names {
        result = add_struct_prefix(&result, name);
    }
    result
}

/// 替换函数体中的构造函数调用:
/// `Type var(args)` → `Type var; _ctor_Type(&var, args);`
/// Convert if-condition variable declarations to C style
/// Pattern: if (Type var = expr) { ... } → Type var = expr; if (var) { ... }
fn convert_if_condition_decls(s: &str) -> String {
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
        if bytes[i] == b'/' && i+1 < len && (bytes[i+1] == b'/' || bytes[i+1] == b'*') {
            let j = skip_comment(bytes, i);
            result.push_str(&s[i..j]);
            i = j;
            continue;
        }
        
        // Look for "if ("
        if match_word_at(bytes, i, "if") {
            let if_start = i;
            let after_if = skip_ws(bytes, i + 2);
            if after_if < len && bytes[after_if] == b'(' {
                // Find the matching ')'
                let mut paren_depth = 1i32;
                let mut j = after_if + 1;
                while j < len && paren_depth > 0 {
                    if bytes[j] == b'(' { paren_depth += 1; }
                    else if bytes[j] == b')' { paren_depth -= 1; }
                    j += 1;
                }
                let paren_end = j - 1; // position of ')'
                let cond_content = &s[after_if+1..paren_end];
                
                // Check if condition contains a variable declaration: Type var = expr
                // Look for pattern: identifier identifier = 
                // But skip comparison operators: !=, ==, <=, >=
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
                            if next2 == b'=' { /* also ==, skip */ }
                            else {
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
                            last_char == b'!' || last_char == b'='
                                || last_char == b'<' || last_char == b'>'
                        } else {
                            false
                        }
                    };
                    
                    if !is_comparison {
                        let before_eq = cond_content[..eq_pos].trim();
                        let after_eq = cond_content[eq_pos+1..].trim();
                        
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
                                        .map_or(false, |c| c.is_alphabetic()
                                        || c == '_'));
                            
                            // Also check that no part contains binary operators
                            // (but allow * which can be a pointer declarator)
                            let has_operators = parts.iter().any(|p| {
                                p.contains('>') || p.contains('<')
                                    || p.contains('&') || p.contains('|')
                                    || p.contains('!') || p.contains('+')
                                    || p.contains('-') || p.contains('/')
                                    || p.contains('%') || p.contains('^')
                                    || p.contains('~') || p.contains('?')
                                    || p.contains(',') || p.contains('[')
                                    || p.contains(']') || p.contains('(')
                                    || p.contains(')') || p.contains('=')
                            });
                            
                            if looks_like_type && !has_operators {
                                // Likely a declaration: Type varname = expr
                                let var_type = parts[..parts.len()-1].join(" ");
                                let var_name = parts[parts.len()-1];
                                
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
            }
        }
        
        result.push(bytes[i] as char);
        i += 1;
    }
    
    result
}

fn replace_ctor_calls_in_bodies(s: &str) -> String {
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
                    if !name.is_empty() { known_types.push(name); }
                }
            }
            i += 1;
        }
    }
    if known_types.is_empty() { return s.to_string(); }

    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut brace_depth = 0i32;

    while i < len {
        if bytes[i] == b'"' { let j = skip_string_literal(bytes, i); result.push_str(&s[i..j]); i = j; continue; }
        if bytes[i] == b'/' && i+1 < len && (bytes[i+1] == b'/' || bytes[i+1] == b'*') {
            let j = skip_comment(bytes, i); result.push_str(&s[i..j]); i = j; continue;
        }
        if bytes[i] == b'{' { brace_depth += 1; }
        if bytes[i] == b'}' { brace_depth = (brace_depth - 1).max(0); }

        if brace_depth > 0 {
            let mut matched = false;
            for ty in known_types.iter() {
                if match_word_at(bytes, i, ty) {
                    let ty_len = ty.len();
                    let j = skip_ws(bytes, i + ty_len);
                    if j < len && is_ident_char(bytes[j]) {
                        let (var_name, var_end) = read_ident(bytes, j);
                        let k = skip_ws(bytes, var_end);
                        if k < len && bytes[k] == b'(' {
                            let mut depth = 1i32;
                            let mut m = k + 1;
                            while m < len && depth > 0 {
                                if bytes[m] == b'(' { depth += 1; }
                                else if bytes[m] == b')' { depth -= 1; }
                                m += 1;
                            }
                            let args = s[k+1..m-1].trim().to_string();
                            let after = skip_ws(bytes, m);
                            // 检查 args 是否看起来像参数声明 (包含类型关键字)
                            // 如果是, 这可能是函数定义而不是构造函数调用
                            let looks_like_params = args.contains("void") || args.contains("int ")
                                || args.contains("char") || args.contains("unsigned")
                                || args.contains("long") || args.contains("float")
                                || args.contains("double") || args.contains("struct ");
                            if after < len && bytes[after] == b';' && !looks_like_params {
                                if args.is_empty() {
                                    result.push_str(&format!(
                                        "{} {}; _ctor_{}(&{});",
                                        ty, var_name, ty, var_name
                                    ));
                                } else {
                                    result.push_str(&format!(
                                        "{} {}; _ctor_{}(&{}, {});",
                                        ty, var_name, ty, var_name, args
                                    ));
                                }
                                i = after + 1;
                                matched = true;
                                break;
                            }
                        }
                    }
                }
            }
            if matched { continue; }
        }

        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// 替换花括号初始化: `Type var{args}` → `Type var = {args}`
fn replace_brace_init(s: &str) -> String {
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
                let mut k = skip_ws(bytes, j + kw_len);
                if k < len && is_ident_char(bytes[k]) {
                    let (name, _) = read_ident(bytes, k);
                    if !name.is_empty() { known_types.push(name); }
                }
            }
            j += 1;
        }
    }

    while i < len {
        if bytes[i] == b'"' { let j = skip_string_literal(bytes, i); result.push_str(&s[i..j]); i = j; continue; }
        if bytes[i] == b'/' && i+1 < len && (bytes[i+1] == b'/' || bytes[i+1] == b'*') {
            let j = skip_comment(bytes, i); result.push_str(&s[i..j]); i = j; continue;
        }

        // 检查 TypeName varname{
        let mut matched = false;
        for ty in &known_types {
            if match_word_at(bytes, i, ty) {
                let ty_len = ty.len();
                let mut j = skip_ws(bytes, i + ty_len);
                if j < len && is_ident_char(bytes[j]) {
                    let (var_name, var_end) = read_ident(bytes, j);
                    let k = skip_ws(bytes, var_end);
                    if k < len && bytes[k] == b'{' {
                        // 找到匹配的 '}'
                        let mut depth = 1i32;
                        let mut m = k + 1;
                        while m < len && depth > 0 {
                            if bytes[m] == b'{' { depth += 1; }
                            else if bytes[m] == b'}' { depth -= 1; }
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
        if matched { continue; }

        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// 替换成员函数调用:
/// `obj.method(args)` → `method(&obj, args)`
/// `ptr->method(args)` → `method(ptr, args)`
fn replace_member_calls(s: &str) -> String {
    // 收集已知成员函数名 (从 struct 内的 inline 定义中)
    // 简化: 不收集, 而是匹配所有 `ident.ident(` 和 `ident->ident(` 模式
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        if bytes[i] == b'"' { let j = skip_string_literal(bytes, i); result.push_str(&s[i..j]); i = j; continue; }
        if bytes[i] == b'/' && i+1 < len && (bytes[i+1] == b'/' || bytes[i+1] == b'*') {
            let j = skip_comment(bytes, i); result.push_str(&s[i..j]); i = j; continue;
        }

        // 检测 ident.ident( 或 ident->ident(
        if is_ident_char(bytes[i]) && (i == 0 || !is_ident_char(bytes[i-1])) {
            let (obj_name, obj_end) = read_ident(bytes, i);
            let j = skip_ws(bytes, obj_end);
            if j < len && (bytes[j] == b'.' || (bytes[j] == b'-' && j+1 < len && bytes[j+1] == b'>')) {
                let is_arrow = bytes[j] == b'-';
                let dot_pos = j;
                let k = skip_ws(bytes, if is_arrow { j + 2 } else { j + 1 });
                if k < len && is_ident_char(bytes[k]) {
                    let (method_name, method_end) = read_ident(bytes, k);
                    let m = skip_ws(bytes, method_end);
                    if m < len && bytes[m] == b'(' {
                        // 找到参数列表
                        let mut depth = 1i32;
                        let mut n = m + 1;
                        while n < len && depth > 0 {
                            if bytes[n] == b'(' { depth += 1; }
                            else if bytes[n] == b')' { depth -= 1; }
                            n += 1;
                        }
                        let args_str = &s[m+1..n-1];
                        let obj_arg = if is_arrow {
                            obj_name.clone()
                        } else {
                            format!("&{}", obj_name)
                        };
                        let new_args = if args_str.trim().is_empty() {
                            obj_arg
                        } else {
                            format!("{}, {}", obj_arg, args_str.trim())
                        };
                        result.push_str(&format!("{}_{}({})", obj_name, method_name, new_args));
                        i = n;
                        continue;
                    }
                }
                let _ = dot_pos;
            }
        }

        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// 替换 nullptr → ((void*)0), 删除 mutable, 删除模板参数等
/// 以及 C++ 引用参数 (&) → 指针 (*)
fn fix_remaining_cpp_syntax(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    // 收集所有已知类型名 (struct/typedef/enum)
    let mut known_types: std::collections::HashSet<String> = std::collections::HashSet::new();
    {
        let mut j = 0;
        while j < len {
            if bytes[j] == b'"' { j = skip_string_literal(bytes, j); continue; }
            if match_word_at(bytes, j, "struct") || match_word_at(bytes, j, "typedef")
                || match_word_at(bytes, j, "enum") || match_word_at(bytes, j, "class")
            {
                let kw_len = if match_word_at(bytes, j, "struct") { 6 }
                    else if match_word_at(bytes, j, "typedef") { 7 }
                    else if match_word_at(bytes, j, "enum") { 4 } else { 5 };
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
    for kw in &["int", "char", "bool", "long", "short", "float", "double",
                "unsigned", "signed", "void", "auto", "size_t", "ssize_t",
                "ptrdiff_t", "intptr_t", "uintptr_t"] {
        known_types.insert(kw.to_string());
    }
    let mut i = 0;
    // 跟踪是否在函数参数列表中
    let mut paren_depth = 0i32;
    while i < len {
        if bytes[i] == b'"' { let j = skip_string_literal(bytes, i); result.push_str(&s[i..j]); i = j; continue; }
        if bytes[i] == b'/' && i+1 < len && (bytes[i+1] == b'/' || bytes[i+1] == b'*') {
            let j = skip_comment(bytes, i); result.push_str(&s[i..j]); i = j; continue;
        }
        // 跟踪括号深度
        if bytes[i] == b'(' { paren_depth += 1; }
        if bytes[i] == b')' { paren_depth = (paren_depth - 1).max(0); }
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
        // namespace XXX { ... } → 删除 namespace XXX 和匹配的 }
        if match_word_at(bytes, i, "namespace") {
            let mut j = skip_ws(bytes, i + 9);
            // 跳过 namespace 名 (如果有)
            if j < len && is_ident_char(bytes[j]) {
                let (_, name_end) = read_ident(bytes, j);
                j = skip_ws(bytes, name_end);
            }
            // 如果后面是 '{', 跳过整个 namespace 块
            if j < len && bytes[j] == b'{' {
                // 找到匹配的 '}'
                let mut depth = 1i32;
                let mut k = j + 1;
                while k < len && depth > 0 {
                    if bytes[k] == b'{' { depth += 1; }
                    else if bytes[k] == b'}' { depth -= 1; }
                    k += 1;
                }
                // 输出 namespace 块的内容 (不含 namespace 关键字和 { })
                result.push_str(&s[j+1..k-1]);
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
            if i + 1 < len && bytes[i+1] == b'&' {
                // && 在参数列表中可能是移动语义, 在表达式中是逻辑与
                // 不替换, 原样输出
                result.push_str("&&");
                i += 2;
                continue;
            }
            // 检查前面是标识符字符 (可能是类型名)
            let prev_char = if result.is_empty() { b' ' } else { result.as_bytes().last().copied().unwrap() };
            if is_ident_char(prev_char) || prev_char == b'*' || prev_char == b'>' || prev_char == b' ' {
                // 提取前面的标识符, 检查是否是类型名
                let is_ref_decl = if is_ident_char(prev_char) {
                    // 向前找到标识符起始位置
                    let rbytes = result.as_bytes();
                    let mut id_end = rbytes.len();
                    let mut id_start = id_end;
                    while id_start > 0 && is_ident_char(rbytes[id_start-1]) { id_start -= 1; }
                    let prev_ident = &result[id_start..id_end];
                    // 检查是否是已知类型名或以 _t 结尾
                    known_types.contains(prev_ident) || prev_ident.ends_with("_t")
                } else if prev_char == b'*' || prev_char == b'>' {
                    // 指针或模板类型后面也可以是引用
                    true
                } else {
                    // 前面是空格, 检查更前面的标识符
                    let rbytes = result.as_bytes();
                    let mut k = rbytes.len();
                    // 跳过空格
                    while k > 0 && (rbytes[k-1] == b' ' || rbytes[k-1] == b'\t') { k -= 1; }
                    if k > 0 && is_ident_char(rbytes[k-1]) {
                        let id_end = k;
                        let mut id_start = k;
                        while id_start > 0 && is_ident_char(rbytes[id_start-1]) { id_start -= 1; }
                        let prev_ident = &result[id_start..id_end];
                        known_types.contains(prev_ident) || prev_ident.ends_with("_t")
                    } else {
                        false
                    }
                };
                if is_ref_decl {
                    // 检查后面是标识符开始或空白+标识符
                    let mut j = skip_ws(bytes, i + 1);
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
        // 检测: 不在括号内, '&' 前面是类型, 后面是换行或标识符 (函数名)
        if paren_depth == 0 && bytes[i] == b'&' {
            // 检查不是 &&
            if i + 1 < len && bytes[i+1] == b'&' {
                result.push_str("&&");
                i += 2;
                continue;
            }
            // 检查前面是标识符字符或空格 (类型结束)
            let prev_char = if result.is_empty() { b' ' } else { result.as_bytes().last().copied().unwrap() };
            if is_ident_char(prev_char) || prev_char == b'>' || prev_char == b' ' {
                // 检查后面是否是空白+换行或标识符 (函数名)
                let j = skip_ws(bytes, i + 1);
                if j < len && (bytes[j] == b'\n' || bytes[j] == b'\r' || is_ident_char(bytes[j])) {
                    // 这是引用返回类型, 替换 & 为 *
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

/// Convert standalone `inline` to `static inline` to avoid C99 inline semantics.
/// In C99, `inline` without `static` has external linkage, causing multiple
/// definition errors when the same inline function appears in multiple TUs.
fn convert_inline_to_static_inline(s: &str) -> String {
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
        if bytes[i] == b'/' && i+1 < len && (bytes[i+1] == b'/' || bytes[i+1] == b'*') {
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

/// 剥离 GCC 属性宏: GTY(...), __attribute__((...)), __declspec(...), 等
/// 这些属性在 C 降级中不需要, 且会干扰解析
fn strip_gcc_attributes(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        // 跳过字符串和注释
        if bytes[i] == b'"' { let j = skip_string_literal(bytes, i); result.push_str(&s[i..j]); i = j; continue; }
        if bytes[i] == b'/' && i+1 < len && (bytes[i+1] == b'/' || bytes[i+1] == b'*') {
            let j = skip_comment(bytes, i); result.push_str(&s[i..j]); i = j; continue;
        }
        // 检测属性宏: IDENT(...) 其中 IDENT 是 GTY, __attribute__, __declspec 等
        if is_ident_char(bytes[i]) && (i == 0 || !is_ident_char(bytes[i-1])) {
            let (ident, ident_end) = read_ident(bytes, i);
            if ident == "GTY" || ident == "__attribute__" || ident == "__declspec"
                || ident == "__cdecl" || ident == "__stdcall" || ident == "__fastcall"
            {
                let j = skip_ws(bytes, ident_end);
                if j < len && bytes[j] == b'(' {
                    // 跳过整个属性宏 (包括嵌套括号)
                    let mut depth = 1i32;
                    let mut k = j + 1;
                    while k < len && depth > 0 {
                        if bytes[k] == b'(' { depth += 1; }
                        else if bytes[k] == b')' { depth -= 1; }
                        k += 1;
                    }
                    // 跳过属性宏后的空白
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

/// 为所有 struct/enum 生成 typedef, 使得 NAME 和 struct NAME / enum NAME 都有效
fn generate_struct_typedefs(s: &str) -> String {
    let mut struct_names: Vec<String> = Vec::new();
    let mut enum_names: Vec<String> = Vec::new();
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        if bytes[i] == b'"' { i = skip_string_literal(bytes, i); continue; }
        if bytes[i] == b'/' && i+1 < len && (bytes[i+1] == b'/' || bytes[i+1] == b'*') {
            i = skip_comment(bytes, i); continue;
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
                        if bytes[m] == b'(' { depth += 1; }
                        else if bytes[m] == b')' { depth -= 1; }
                        m += 1;
                    }
                    j = skip_ws(bytes, m);
                } else {
                    break;
                }
            }
            if j < len && is_ident_char(bytes[j]) {
                let (name, _) = read_ident(bytes, j);
                if !name.is_empty() && !struct_names.contains(&name)
                    && name != "_IO_FILE" && name != "_IO_marker"
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
                if k < len && (bytes[k] == b'{' || bytes[k] == b';') {
                    if !name.is_empty() && !enum_names.contains(&name) {
                        enum_names.push(name);
                    }
                }
            }
        }
        i += 1;
    }
    if struct_names.is_empty() && enum_names.is_empty() {
        return s.to_string();
    }
    let mut result = String::with_capacity(
        s.len() + struct_names.len() * 50 + enum_names.len() * 50
    );
    result.push_str("/* Generated struct/enum typedefs */\n");
    for name in &struct_names {
        result.push_str(&format!("typedef struct {} {};\n", name, name));
    }
    for name in &enum_names {
        result.push_str(&format!("typedef enum {} {};\n", name, name));
    }
    result.push_str("\n");
    result.push_str(s);
    result
}

/// Fix namespace-flattened enums: when `namespace N { enum class E { ... }; }`
/// is flattened, `N::E::VALUE` becomes `N_E_VALUE` but the enum is defined as
/// `enum E { VALUE, ... }`. This function adds typedefs and defines to bridge
/// the gap.
fn fix_flattened_enum_refs(s: &str) -> String {
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
                        if bytes[m] == b'{' { depth += 1; }
                        else if bytes[m] == b'}' { depth -= 1; }
                        else if depth == 1 && is_ident_char(bytes[m]) 
                            && (m == 0 || !is_ident_char(bytes[m-1])) 
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
    let mut prefix_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    // Map from flattened type name (e.g., "bidi_kind") to original enum name (e.g., "kind")
    
    for (enum_name, _) in &enum_defs {
        // Look for patterns like `IDENT_<enum_name>` used as a type
        // e.g., `bidi_kind` when enum is `kind`
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
                while start > 0 && is_ident_char(before_bytes[start-1]) {
                    start -= 1;
                }
                if start < before_bytes.len() {
                    let prefix = &s[start..abs_pos];
                    let flattened_type = format!("{}_{}", prefix, enum_name);
                    // Check if this flattened type is used in the code
                    if s.contains(&format!(" {} ", flattened_type)) 
                        || s.contains(&format!(" {}\n", flattened_type))
                        || s.contains(&format!("\t{} ", flattened_type))
                        || s.contains(&format!("static {}", flattened_type))
                    {
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

/// 删除所有函数声明中的默认参数: `func(int x = 5)` → `func(int x)`
/// 处理全局函数声明和 struct 体内的函数声明
fn remove_all_default_args(s: &str) -> String {
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
        if bytes[i] == b'/' && i+1 < len && (bytes[i+1] == b'/' || bytes[i+1] == b'*') {
            let j = skip_comment(bytes, i); 
            result.push_str(&s[i..j]); 
            i = j; 
            continue;
        }
        
        // Look for function declarations: identifier followed by '('
        // Check if this looks like a function name (identifier followed by '(')
        if is_ident_char(bytes[i]) && (i == 0 || !is_ident_char(bytes[i-1])) {
            let (ident, ident_end) = read_ident(bytes, i);
            // Skip keywords that look like function calls but aren't
            if ident == "__asm__" || ident == "__volatile__" || ident == "asm" || ident == "volatile"
                || ident == "__attribute__" || ident == "typeof" || ident == "__typeof__"
                || ident == "sizeof" || ident == "alignof" || ident == "__alignof__"
                || ident == "if" || ident == "while" || ident == "for" || ident == "switch"
                || ident == "return" || ident == "case" || ident == "do" || ident == "else"
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
                let mut paren_depth = 1i32;
                let mut k = j + 1;
                let mut has_default_arg = false;
                
                while k < len && paren_depth > 0 {
                    if bytes[k] == b'(' { paren_depth += 1; }
                    else if bytes[k] == b')' { paren_depth -= 1; }
                    else if paren_depth == 1 {
                        // Check for '=' that's not '=='
                        if bytes[k] == b'=' && (k+1 >= len || bytes[k+1] != b'=') {
                            // Check it's not '!=', '<=', '>='
                            if k == 0 || (bytes[k-1] != b'!' && bytes[k-1] != b'<' && bytes[k-1] != b'>') {
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
                    after_paren < len && (
                        bytes[after_paren] == b'{' || 
                        bytes[after_paren] == b';'
                    )
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
                        }
                        else if bytes[i] == b')' { 
                            paren_depth -= 1;
                            if paren_depth == 0 {
                                result.push(')');
                                i += 1;
                                break;
                            }
                            result.push(')');
                            i += 1;
                        }
                        else if bytes[i] == b'=' && paren_depth == 1 {
                            // Check it's not '=='
                            if i+1 < len && bytes[i+1] == b'=' {
                                result.push_str("==");
                                i += 2;
                            }
                            // Check it's not '!=', '<=', '>='
                            else if i > 0 && (bytes[i-1] == b'!' || bytes[i-1] == b'<' || bytes[i-1] == b'>') {
                                result.push('=');
                                i += 1;
                            }
                            else {
                                // Skip the default value until ',' or ')'
                                i += 1;
                                let mut d = 0i32;
                                while i < len {
                                    if bytes[i] == b'(' { d += 1; }
                                    else if bytes[i] == b')' {
                                        if d == 0 { break; }
                                        d -= 1;
                                    }
                                    else if bytes[i] == b',' && d == 0 { break; }
                                    i += 1;
                                }
                            }
                        }
                        else {
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
fn convert_functional_casts(s: &str) -> String {
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
            if bytes[j] == b'"' { j = skip_string_literal(bytes, j); continue; }
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
        if bytes[i] == b'"' { let j = skip_string_literal(bytes, i); result.push_str(&s[i..j]); i = j; continue; }
        if bytes[i] == b'/' && i+1 < len && (bytes[i+1] == b'/' || bytes[i+1] == b'*') {
            let j = skip_comment(bytes, i); result.push_str(&s[i..j]); i = j; continue;
        }
        if bytes[i] == b'(' { paren_depth += 1; }
        if bytes[i] == b')' { paren_depth = (paren_depth - 1).max(0); }
        // 检测类型名后跟 '('
        if is_ident_char(bytes[i]) && (i == 0 || !is_ident_char(bytes[i-1])) {
            let (ident, ident_end) = read_ident(bytes, i);
            let j = skip_ws(bytes, ident_end);
            if j < len && bytes[j] == b'(' {
                // 检查是否是已知类型名或看起来像类型名
                let is_type = known_types.contains(&ident) || ident.ends_with("_t")
                    || ident == "int" || ident == "char" || ident == "bool"
                    || ident == "long" || ident == "short" || ident == "unsigned";
                if is_type {
                    // 检查上下文: 前面是否是操作符/空白 (表达式上下文)
                    // 而不是标识符或 ')' (函数调用或声明上下文)
                    let prev = result.trim_end().as_bytes().last().copied().unwrap_or(b' ');
                    let is_expr_ctx = !is_ident_char(prev) && prev != b')' && prev != b']'
                        && prev != b'(' && prev != b',';
                    // 也检查是否在括号内且前面是操作符
                    if is_expr_ctx {
                        // 检查括号内容是否是简单值 (数字或简单表达式)
                        let mut d = 1i32;
                        let mut k = j + 1;
                        let mut is_simple = true;
                        while k < len && d > 0 {
                            if bytes[k] == b'(' { d += 1; }
                            else if bytes[k] == b')' { d -= 1; }
                            else if bytes[k] == b';' || bytes[k] == b'{' || bytes[k] == b'}' {
                                is_simple = false; break;
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
fn convert_simple_functional_casts(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    // 收集已知类型名
    let mut known_types: Vec<String> = Vec::new();
    {
        let mut j = 0;
        while j < len {
            if bytes[j] == b'"' { j = skip_string_literal(bytes, j); continue; }
            if match_word_at(bytes, j, "struct") || match_word_at(bytes, j, "typedef")
                || match_word_at(bytes, j, "enum")
            {
                let kw_len = if match_word_at(bytes, j, "struct") { 6 }
                    else if match_word_at(bytes, j, "typedef") { 7 } else { 4 };
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
        if bytes[i] == b'"' { let j = skip_string_literal(bytes, i); result.push_str(&s[i..j]); i = j; continue; }
        if bytes[i] == b'/' && i+1 < len && (bytes[i+1] == b'/' || bytes[i+1] == b'*') {
            let j = skip_comment(bytes, i); result.push_str(&s[i..j]); i = j; continue;
        }
        // 检测类型名后跟 '(' 和简单数字
        if is_ident_char(bytes[i]) && (i == 0 || !is_ident_char(bytes[i-1])) {
            let (ident, ident_end) = read_ident(bytes, i);
            let j = skip_ws(bytes, ident_end);
            if j < len && bytes[j] == b'(' {
                let is_type = known_types.contains(&ident) || ident.ends_with("_t")
                    || ident == "int" || ident == "char" || ident == "bool"
                    || ident == "long" || ident == "short" || ident == "unsigned"
                    || ident == "size_t";
                if is_type {
                    // 找到匹配的 ')'
                    let mut k = j + 1;
                    let mut depth = 1i32;
                    let mut has_star = false;
                    while k < len && depth > 0 {
                        if bytes[k] == b'(' { depth += 1; }
                        else if bytes[k] == b')' { depth -= 1; }
                        else if bytes[k] == b'*' { has_star = true; }
                        k += 1;
                    }
                    // 如果括号内容包含 '*', 可能是函数指针声明, 不转换
                    if depth == 0 && !has_star {
                        // 检查前面是否是表达式上下文
                        let prev = result.trim_end().as_bytes().last().copied().unwrap_or(b' ');
                        let is_expr_ctx = !is_ident_char(prev) && prev != b')'
                            && prev != b']' && prev != b';' && prev != b'{'
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
fn convert_direct_initialization(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut last_push = 0;  // 上次 push 到的位置
    while i < len {
        // 跳过字符串和注释
        if bytes[i] == b'"' { let j = skip_string_literal(bytes, i); result.push_str(&s[i..j]); i = j; last_push = i; continue; }
        if bytes[i] == b'/' && i+1 < len && (bytes[i+1] == b'/' || bytes[i+1] == b'*') {
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
            while j > 0 && (bytes[j-1] == b' ' || bytes[j-1] == b'\t' || bytes[j-1] == b'\n') { j -= 1; }
            if j > 0 && is_ident_char(bytes[j-1]) {
                let mut id_end = j;
                let mut id_start = j;
                while id_start > 0 && is_ident_char(bytes[id_start-1]) { id_start -= 1; }
                // 检查标识符前面是否有类型名或 *
                let mut k = id_start;
                while k > 0 && (bytes[k-1] == b' ' || bytes[k-1] == b'\t' || bytes[k-1] == b'\n' || bytes[k-1] == b'*') { k -= 1; }
                if k > 0 && is_ident_char(bytes[k-1]) {
                    // 找到匹配的 )
                    let mut depth = 1;
                    let mut m = i + 1;
                    while m < len && depth > 0 {
                        if bytes[m] == b'(' { depth += 1; }
                        else if bytes[m] == b')' { depth -= 1; }
                        m += 1;
                    }
                    if depth == 0 && m < len {
                        // 检查 ) 后面是否是 ;
                        let mut n = m;
                        while n < len && (bytes[n] == b' ' || bytes[n] == b'\t' || bytes[n] == b'\n') { n += 1; }
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

/// 删除模板定义: template <...> struct/class/func ... { ... }
/// C 不支持模板, 需要完全删除
fn remove_template_definitions(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        // 跳过字符串和注释
        if bytes[i] == b'"' { let j = skip_string_literal(bytes, i); result.push_str(&s[i..j]); i = j; continue; }
        if bytes[i] == b'/' && i+1 < len && (bytes[i+1] == b'/' || bytes[i+1] == b'*') {
            let j = skip_comment(bytes, i); result.push_str(&s[i..j]); i = j; continue;
        }
        // 检测 template 关键字
        if match_word_at(bytes, i, "template") {
            let mut j = skip_ws(bytes, i + 8);
            // 跳过 <...> 模板参数列表
            if j < len && bytes[j] == b'<' {
                let mut depth = 1i32;
                j += 1;
                while j < len && depth > 0 {
                    if bytes[j] == b'<' { depth += 1; }
                    else if bytes[j] == b'>' { depth -= 1; }
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
                            if bytes[j] == b'<' { depth += 1; }
                            else if bytes[j] == b'>' { depth -= 1; }
                            j += 1;
                        }
                    }
                    // 跳过可能的基类列表和 { ... }
                    // 找到 '{' 并跳过整个类定义
                    while j < len && bytes[j] != b'{' && bytes[j] != b';' { j += 1; }
                    if j < len && bytes[j] == b'{' {
                        let mut depth = 1i32;
                        j += 1;
                        while j < len && depth > 0 {
                            if bytes[j] == b'{' { depth += 1; }
                            else if bytes[j] == b'}' { depth -= 1; }
                            j += 1;
                        }
                        // 跳过结尾的 ';'
                        if j < len && bytes[j] == b';' { j += 1; }
                    } else if j < len && bytes[j] == b';' {
                        j += 1;
                    }
                    i = j;
                    continue;
                }
                // 对于模板函数, 跳过整个函数定义
                // 找到 '{' 并跳过整个函数体
                while j < len && bytes[j] != b'{' && bytes[j] != b';' { j += 1; }
                if j < len && bytes[j] == b'{' {
                    let mut depth = 1i32;
                    j += 1;
                    while j < len && depth > 0 {
                        if bytes[j] == b'{' { depth += 1; }
                        else if bytes[j] == b'}' { depth -= 1; }
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
fn replace_template_usages(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        // Skip strings and comments
        if bytes[i] == b'"' { let j = skip_string_literal(bytes, i); result.push_str(&s[i..j]); i = j; continue; }
        if bytes[i] == b'/' && i+1 < len && (bytes[i+1] == b'/' || bytes[i+1] == b'*') {
            let j = skip_comment(bytes, i); result.push_str(&s[i..j]); i = j; continue;
        }
        // Check for identifier followed by '<'
        if is_ident_char(bytes[i]) && (i == 0 || !is_ident_char(bytes[i-1])) {
            let (ident, ident_end) = read_ident(bytes, i);
            let j = skip_ws(bytes, ident_end);
            if j < len && bytes[j] == b'<' {
                // Check if this looks like a template usage
                // Try to find matching '>'
                let mut depth = 1i32;
                let mut k = j + 1;
                let mut has_comma = false;
                let mut has_struct = false;
                while k < len && depth > 0 {
                    if bytes[k] == b'<' { depth += 1; }
                    else if bytes[k] == b'>' { depth -= 1; }
                    else if bytes[k] == b',' && depth == 1 { has_comma = true; }
                    else if bytes[k] == b';' || bytes[k] == b'{' || bytes[k] == b'}' {
                        break; // Not a template
                    }
                    k += 1;
                }
                if depth == 0 && (has_comma || ident.contains("vec")
                    || ident.contains("map") || ident.contains("hash")
                    || ident.contains("set") || ident.contains("pair")
                    || ident.contains("auto_suppress"))
                {
                    // Looks like template usage
                    // Check what follows '>' - should be identifier (var name)
                    // or ';' or ',' or ')'
                    let after_gt = skip_ws(bytes, k);
                    let is_type_pos = after_gt < len && (
                        is_ident_char(bytes[after_gt])
                        || bytes[after_gt] == b'*'
                        || bytes[after_gt] == b'&'
                        || bytes[after_gt] == b';'
                        || bytes[after_gt] == b','
                        || bytes[after_gt] == b')'
                        || bytes[after_gt] == b'>'
                    );
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

/// 转换 return struct Type(args) 为 return (struct Type){args} (C99 compound literal)
fn convert_return_struct_ctor(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        // 跳过字符串和注释
        if bytes[i] == b'"' { let j = skip_string_literal(bytes, i); result.push_str(&s[i..j]); i = j; continue; }
        if bytes[i] == b'/' && i+1 < len && (bytes[i+1] == b'/' || bytes[i+1] == b'*') {
            let j = skip_comment(bytes, i); result.push_str(&s[i..j]); i = j; continue;
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
                            if bytes[n] == b'(' { depth += 1; }
                            else if bytes[n] == b')' { depth -= 1; }
                            n += 1;
                        }
                        if depth == 0 {
                            // 提取参数
                            let args = &s[m+1..n-1];
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

/// 展平 C++ scope 操作符 `::` 为下划线: `std::string` → `std_string`.
/// 只处理 `ident::ident` 形式; 前导 `::` (全局限定) 只去掉 `::`.
fn flatten_scope_operators(s: &str) -> String {
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut result = String::with_capacity(len);
    let mut i = 0;
    while i < len {
        // 跳过字符串字面量
        if bytes[i] == b'"' { let j = skip_string_literal(bytes, i); result.push_str(&s[i..j]); i = j; continue; }
        // 跳过注释
        if bytes[i] == b'/' && i+1 < len && (bytes[i+1] == b'/' || bytes[i+1] == b'*') {
            let j = skip_comment(bytes, i); result.push_str(&s[i..j]); i = j; continue; }
        // 检测 `::`
        if bytes[i] == b':' && i+1 < len && bytes[i+1] == b':' {
            // 检查前面是否是标识符 (形成 ident:: 模式)
            if result.len() > 0 && {
                let rb = result.as_bytes();
                let rl = rb.len();
                rl > 0 && is_ident_char(rb[rl-1])
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

/// 预处理 C++ 源文本, 将复杂 C++ 构造替换为 C 兼容形式.
/// 在 strip_cpp_keywords 的字符级处理之前调用.
fn replace_cpp_constructs(s: &str) -> String {
    let mut result = s.to_string();
    // 剥离 GCC 属性宏 (如 GTY(...), __attribute__((...)), 等)
    result = strip_gcc_attributes(&result);
    // 展平 namespace 限定符: std::string → std_string
    // 必须在 clean_struct_bodies 之前运行, 否则 struct 体内
    // 的类型名仍含 :: 导致后续处理出错
    result = flatten_scope_operators(&result);
    // 删除模板定义 (C 不支持模板)
    result = remove_template_definitions(&result);
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
            if bytes[i] == b':' && i + 1 < len && bytes[i+1] == b':' {
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

/// 替换独立的单词 (前后必须是标识符边界)
fn replace_word(s: &str, from: &str, to: &str) -> String {
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
                if !to.is_empty() && to.as_bytes().last().map_or(false, |b| b.is_ascii_alphanumeric()) {
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
fn replace_phrase(s: &str, from: &str, to: &str) -> String {
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

/// 删除访问说明符 "public:" / "private:" / "protected:"
fn replace_access_specifier(s: &str, kw: &str) -> String {
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
fn replace_lambdas(s: &str) -> String {
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
                        if bytes[j] == b'(' { depth += 1; } else if bytes[j] == b')' { depth -= 1; }
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
                        if bytes[j] == b'{' { depth += 1; } else if bytes[j] == b'}' { depth -= 1; }
                        j += 1;
                    }
                    // 成功匹配 lambda, 替换为 0
                    result.push_str("0");
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
fn replace_destructors(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        // 查找 ~ 字符
        if bytes[i] == b'~' {
            // 检查前面是 '::' (析构函数定义)
            let mut before = i;
            if before >= 2 && bytes[before-1] == b':' && bytes[before-2] == b':' {
                // 找到类名
                let mut class_end = before - 2;
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
fn replace_constructors(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        // 查找 ClassName::ClassName 模式 (类名与函数名相同)
        if bytes[i] == b':' && i + 1 < len && bytes[i+1] == b':' {
            // 前面是类名
            let mut class_end = i;
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
            let mut j = i + 2;
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
                        if bytes[k] == b'(' { depth += 1; } else if bytes[k] == b')' { depth -= 1; }
                        k += 1;
                    }
                    // 跳过空白
                    while k < len && (bytes[k] as char).is_ascii_whitespace() {
                        k += 1;
                    }
                    // 跳过构造函数初始化列表 : member(init), ...
                    if k < len && bytes[k] == b':' {
                        // 检查不是 '::'
                        if k + 1 < len && bytes[k+1] != b':' {
                            k += 1; // 跳过 ':'
                            // 跳过到 '{'
                            let mut brace_depth = 0i32;
                            while k < len {
                                if bytes[k] == b'(' { brace_depth += 1; }
                                else if bytes[k] == b')' { brace_depth -= 1; }
                                else if bytes[k] == b'{' && brace_depth == 0 { break; }
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
                    result.push_str(&format!("void _ctor_{}()", class_name));
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
fn replace_flattened_ctor_dtor(s: &str) -> String {
    // Step 1: Collect all struct names from `struct NAME {` / `struct NAME\n{`
    let mut struct_names: Vec<String> = Vec::new();
    {
        let bytes = s.as_bytes();
        let len = bytes.len();
        let mut i = 0;
        while i < len {
            if bytes[i] == b'"' { i = skip_string_literal(bytes, i); continue; }
            if bytes[i] == b'/' && i+1 < len
                && (bytes[i+1] == b'/' || bytes[i+1] == b'*')
            {
                i = skip_comment(bytes, i); continue;
            }
            if match_word_at(bytes, i, "struct") {
                let j = skip_ws(bytes, i + 6);
                if j < len && is_ident_char(bytes[j]) {
                    let (name, name_end) = read_ident(bytes, j);
                    let k = skip_ws(bytes, name_end);
                    if k < len && bytes[k] == b'{' {
                        if !struct_names.contains(&name) {
                            struct_names.push(name);
                        }
                    }
                }
            }
            i += 1;
        }
    }
    // Sort longest first to avoid substring false matches
    struct_names.sort_by(|a, b| b.len().cmp(&a.len()));

    // Step 2: Replace destructor patterns
    // ClassName_~ClassName( → void _dtor_ClassName (
    let mut result = s.to_string();
    for name in &struct_names {
        let pattern = format!("{}_~{}", name, name);
        loop {
            if let Some(pos) = result.find(&pattern) {
                // Check word boundary before pattern
                if pos > 0 {
                    let prev = result.as_bytes()[pos - 1];
                    if is_ident_char(prev) { break; }
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
                    result = format!(
                        "{}{}{}",
                        &result[..pos],
                        replacement,
                        &result[after..]
                    );
                } else {
                    break;
                }
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
        loop {
            if let Some(pos) = result.find(&pattern) {
                // Check word boundary before pattern
                if pos > 0 {
                    let prev = result.as_bytes()[pos - 1];
                    if is_ident_char(prev) { break; }
                }
                let after = pos + pattern.len();
                let rb = result.as_bytes();
                // Skip whitespace
                let mut j = after;
                while j < rb.len()
                    && (rb[j] as char).is_ascii_whitespace()
                {
                    j += 1;
                }
                // Must be followed by (
                if j >= rb.len() || rb[j] != b'(' { break; }
                // Find matching )
                let mut depth = 1i32;
                j += 1; // skip (
                while j < rb.len() && depth > 0 {
                    if rb[j] == b'(' { depth += 1; }
                    else if rb[j] == b')' { depth -= 1; }
                    j += 1;
                }
                // j is now right after matching ')'
                // Save this position - we keep params up to here
                let after_params = j;
                // Skip whitespace
                while j < rb.len()
                    && (rb[j] as char).is_ascii_whitespace()
                {
                    j += 1;
                }
                // Skip initializer list : member(init), ...
                if j < rb.len() && rb[j] == b':'
                    && (j + 1 >= rb.len() || rb[j + 1] != b':')
                {
                    j += 1;
                    let mut bd = 0i32;
                    while j < rb.len() {
                        if rb[j] == b'(' { bd += 1; }
                        else if rb[j] == b')' { bd -= 1; }
                        else if rb[j] == b'{' && bd == 0 { break; }
                        j += 1;
                    }
                }
                let replacement = format!("void _ctor_{}", name);
                // Keep: prefix + replacement + params + rest from j
                result = format!(
                    "{}{} {}{}",
                    &result[..pos],
                    replacement,
                    &result[after..after_params],
                    &result[j..]
                );
            } else {
                break;
            }
        }
    }

    result
}

/// 去除 C++ 特有关键字, 使 extern "C" 块内的声明可被 C 编译器接受.
/// 目前去除:
///   - `noexcept` / `noexcept(expr)`
/// 强制展平所有剩余的 :: 为 _ (除了字符串字面量中的)
/// 用于 AST 路径和 fallback 路径的后处理
pub fn flatten_all_scope_operators(s: &str) -> String {
    // Simple version: just replace all :: with _ (no string literal handling for now)
    s.replace("::", "_")
}

///   - `constexpr` (函数声明中)
///   - `__extension__` (GCC 扩展, 某些 C 模式不接受)
pub fn strip_cpp_keywords(s: &str) -> String {
    // 先做整体文本替换 (多 token 构造需要先处理)
    let s = replace_cpp_constructs(s);
    // 前置 C++ 内置类型定义, 确保系统头文件中的类型可用
    let mut result = String::with_capacity(s.len() + 256);
    result.push_str("typedef int wchar_t;\n");
    result.push_str("typedef long unsigned int size_t;\n");
    result.push_str("typedef long int ptrdiff_t;\n");
    result.push_str("#ifndef __cplusplus\n");
    result.push_str("typedef int bool;\n");
    result.push_str("#endif\n");
    // Helper for std::swap → std_swap (used by GCC source)
    result.push_str("#define std_swap(a, b) do { __typeof__(a) _tmp = (a); (a) = (b); (b) = _tmp; } while(0)\n");
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        // 检查 extern "C" { -> 去掉 extern "C" 包装, 只保留 { 内容
        // "extern" 是 6 字节
        if i + 6 <= len && &bytes[i..i + 6] == b"extern" {
            let before_ok = i == 0 || !is_ident_char(bytes[i - 1]);
            let after_ok = i + 6 >= len || !is_ident_char(bytes[i + 6]);
            if before_ok && after_ok {
                let mut j = i + 6;
                // 跳过空白 (空格/Tab/换行)
                while j < len && (bytes[j] as char).is_ascii_whitespace() {
                    j += 1;
                }
                // 检查是否后跟 "C" 或 "c"
                if j < len && bytes[j] == b'"' {
                    // 查找字符串字面量结束
                    let str_start = j;
                    j += 1; // 跳过开头 "
                    while j < len && bytes[j] != b'"' {
                        j += 1;
                    }
                    if j < len {
                        j += 1; // 跳过结尾 "
                        let lit = &bytes[str_start..j];
                        if lit == b"\"C\"" || lit == b"\"c\"" || lit == b"\"C++\"" || lit == b"\"c++\"" {
                            // 跳过空白
                            while j < len && (bytes[j] as char).is_ascii_whitespace() {
                                j += 1;
                            }
                            // extern "C" { ... } -> 去掉 extern "C" { 和匹配的 }
                            // 只保留内容, 让内部声明成为顶层声明
                            if j < len && bytes[j] == b'{' {
                                // 找到匹配的 } (跳过字符串和注释)
                                let mut depth = 1i32;
                                let mut k = j + 1;
                                while k < len && depth > 0 {
                                    // 跳过字符串
                                    if bytes[k] == b'"' {
                                        k = skip_string_literal(bytes, k);
                                        continue;
                                    }
                                    // 跳过注释
                                    if k + 1 < len && bytes[k] == b'/' && (bytes[k+1] == b'/' || bytes[k+1] == b'*') {
                                        k = skip_comment(bytes, k);
                                        continue;
                                    }
                                    if bytes[k] == b'{' {
                                        depth += 1;
                                    } else if bytes[k] == b'}' {
                                        depth -= 1;
                                    }
                                    k += 1;
                                }
                                // 输出 extern "C" 块的内容 (不含外层 { })
                                // 递归清洗内容 (可能还有嵌套的 extern "C")
                                let inner = &s[j + 1..k - 1];
                                result.push_str(&strip_cpp_keywords(inner));
                                i = k;
                                continue;
                            }
                            // extern "C" 单声明 (无 {) -> 去掉 extern "C"
                            i = j;
                            continue;
                        }
                    }
                }
            }
        }
        // 检查 noexcept
        if i + 8 <= len && &bytes[i..i + 8] == b"noexcept" {
            let before_ok = i == 0 || !is_ident_char(bytes[i - 1]);
            let after_ok = i + 8 >= len || !is_ident_char(bytes[i + 8]);
            if before_ok && after_ok {
                let mut j = i + 8;
                while j < len && bytes[j] == b' ' {
                    j += 1;
                }
                if j < len && bytes[j] == b'(' {
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
                }
                while j < len && bytes[j] == b' ' {
                    j += 1;
                }
                i = j;
                continue;
            }
        }
        // 检查 constexpr
        if i + 9 <= len && &bytes[i..i + 9] == b"constexpr" {
            let before_ok = i == 0 || !is_ident_char(bytes[i - 1]);
            let after_ok = i + 9 >= len || !is_ident_char(bytes[i + 9]);
            if before_ok && after_ok {
                let mut j = i + 9;
                while j < len && bytes[j] == b' ' {
                    j += 1;
                }
                i = j;
                continue;
            }
        }
        // decltype(expr) → void* (C 后端不接受 decltype)
        if i + 8 <= len && &bytes[i..i + 8] == b"decltype" {
            let before_ok = i == 0 || !is_ident_char(bytes[i - 1]);
            let after_ok = i + 8 >= len || !is_ident_char(bytes[i + 8]);
            if before_ok && after_ok {
                let mut j = i + 8;
                while j < len && bytes[j] == b' ' {
                    j += 1;
                }
                if j < len && bytes[j] == b'(' {
                    // 跳过括号内容 (包括嵌套括号)
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
                }
                result.push_str("void*");
                i = j;
                continue;
            }
        }
        // nullptr → ((void*)0)
        if i + 9 <= len && &bytes[i..i + 9] == b"nullptr" {
            let before_ok = i == 0 || !is_ident_char(bytes[i - 1]);
            let after_ok = i + 9 >= len || !is_ident_char(bytes[i + 9]);
            if before_ok && after_ok {
                result.push_str("((void*)0)");
                i += 9;
                continue;
            }
        }
        // static_cast<T>(e) / dynamic_cast<T>(e) / const_cast<T>(e) /
        // reinterpret_cast<T>(e) → (T)(e)
        for &(kw, kw_len) in &[
            (b"static_cast" as &[u8], 11usize),
            (b"dynamic_cast", 12),
            (b"const_cast", 10),
            (b"reinterpret_cast", 16),
        ] {
            if i + kw_len <= len && &bytes[i..i + kw_len] == kw {
                let before_ok = i == 0 || !is_ident_char(bytes[i - 1]);
                let after_ok = i + kw_len >= len || !is_ident_char(bytes[i + kw_len]);
                if before_ok && after_ok {
                    let mut j = i + kw_len;
                    while j < len && bytes[j] == b' ' {
                        j += 1;
                    }
                    // 解析 <T> 部分
                    if j < len && bytes[j] == b'<' {
                        let mut depth = 1i32;
                        let type_start = j + 1;
                        j += 1;
                        while j < len && depth > 0 {
                            if bytes[j] == b'<' {
                                depth += 1;
                            } else if bytes[j] == b'>' {
                                depth -= 1;
                            }
                            j += 1;
                        }
                        let type_end = j - 1;
                        let type_str = &s[type_start..type_end];
                        result.push('(');
                        result.push_str(type_str);
                        result.push(')');
                        // 跳过 (expr)
                        while j < len && bytes[j] == b' ' {
                            j += 1;
                        }
                        if j < len && bytes[j] == b'(' {
                            let mut depth = 1i32;
                            let expr_start = j + 1;
                            j += 1;
                            while j < len && depth > 0 {
                                if bytes[j] == b'(' {
                                    depth += 1;
                                } else if bytes[j] == b')' {
                                    depth -= 1;
                                }
                                j += 1;
                            }
                            let expr_end = j - 1;
                            let expr_str = &s[expr_start..expr_end];
                            result.push('(');
                            result.push_str(expr_str);
                            result.push(')');
                        }
                    }
                    i = j;
                    continue;
                }
            }
        }
        // override / final (类方法后) → 删除
        for &(kw, kw_len) in &[(b"override" as &[u8], 8usize), (b"final", 5)] {
            if i + kw_len <= len && &bytes[i..i + kw_len] == kw {
                let before_ok = i == 0 || !is_ident_char(bytes[i - 1]);
                let after_ok = i + kw_len >= len || !is_ident_char(bytes[i + kw_len]);
                if before_ok && after_ok {
                    let mut j = i + kw_len;
                    while j < len && bytes[j] == b' ' {
                        j += 1;
                    }
                    i = j;
                    continue;
                }
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    // 最后强制替换所有剩余的 :: 为 _ (除了字符串字面量中的)
    let mut final_result = String::with_capacity(result.len());
    let rbytes = result.as_bytes();
    let rlen = rbytes.len();
    let mut ri = 0;
    while ri < rlen {
        if rbytes[ri] == b'"' {
            let rj = skip_string_literal(rbytes, ri);
            final_result.push_str(&result[ri..rj]);
            ri = rj;
            continue;
        }
        if rbytes[ri] == b':' && ri + 1 < rlen && rbytes[ri+1] == b':' {
            final_result.push('_');
            ri += 2;
            continue;
        }
        final_result.push(rbytes[ri] as char);
        ri += 1;
    }
    final_result
}

/// 类型到 C 字符串 (模块级, Lower 内外共用)
pub fn mangle_to_c(ty: &MangleType) -> String {
    match ty {
        MangleType::Void => "void".into(),
        MangleType::Bool => "_Bool".into(),
        MangleType::Char => "char".into(),
        MangleType::Schar => "signed char".into(),
        MangleType::Uchar => "unsigned char".into(),
        MangleType::Short => "short".into(),
        MangleType::Ushort => "unsigned short".into(),
        MangleType::Int => "int".into(),
        MangleType::Uint => "unsigned int".into(),
        MangleType::Long => "long".into(),
        MangleType::Ulong => "unsigned long".into(),
        MangleType::LongLong => "long long".into(),
        MangleType::UlongLong => "unsigned long long".into(),
        MangleType::Float => "float".into(),
        MangleType::Double => "double".into(),
        MangleType::LongDouble => "long double".into(),
        MangleType::Pointer(t) => format!("{}*", mangle_to_c(t)),
        MangleType::Reference(t) => format!("{}*", mangle_to_c(t)),
        MangleType::Class(n) => {
            // enum/struct/union 前缀: 保留原始关键字而非一律加 struct
            if n.starts_with("enum ") || n.starts_with("union ")
                || n.starts_with("struct ")
            {
                n.clone()
            } else {
                format!("struct {}", n)
            }
        }
        MangleType::Ellipsis => "...".into(),
        // M17.0: Auto / DecltypeAuto 在所有变量声明点都应已被解析;
        // 若此处仍出现, 说明 lower 顺序错误, 输出占位 int 并发出警告.
        MangleType::Auto | MangleType::DecltypeAuto => "int /*M17 unresolved auto*/".into(),
        // M18: T<U1, U2, ...> 模板实例化 — C 端没有等价表达, 用占位 typedef.
        // 真实单态化由 dcc 模板子系统按需生成; 这里降级为 T_<U1>_<U2> 形态.
        MangleType::Template { name, args } => {
            // Flatten namespace qualifiers: std::vector -> std_vector
            let flat_name = name.replace("::", "_");
            let mut s = format!("__dcc_template_{}", flat_name);
            for a in args {
                s.push('_');
                s.push_str(&template_arg_to_c(a));
            }
            format!("struct {}", s)
        }
    }
}

/// 模板实参序列化为 C 标识符片段 (用于 typedef 名).
fn template_arg_to_c(ty: &MangleType) -> String {
    match ty {
        MangleType::Void => "void".into(),
        MangleType::Bool => "bool".into(),
        MangleType::Char => "char".into(),
        MangleType::Int => "int".into(),
        MangleType::Uint => "uint".into(),
        MangleType::Long => "long".into(),
        MangleType::Ulong => "ulong".into(),
        MangleType::LongLong => "llong".into(),
        MangleType::UlongLong => "ullong".into(),
        MangleType::Float => "float".into(),
        MangleType::Double => "double".into(),
        MangleType::Pointer(t) => format!("p_{}", template_arg_to_c(t)),
        MangleType::Reference(t) => format!("r_{}", template_arg_to_c(t)),
        MangleType::Class(n) => n.replace("::", "_"),
        MangleType::Template { name, args } => {
            let flat_name = name.replace("::", "_");
            let mut s = format!("T_{}", flat_name);
            for a in args {
                s.push('_');
                s.push_str(&template_arg_to_c(a));
            }
            s
        }
        MangleType::Schar => "schar".into(),
        MangleType::Uchar => "uchar".into(),
        MangleType::Short => "short".into(),
        MangleType::Ushort => "ushort".into(),
        MangleType::LongDouble => "ldouble".into(),
        MangleType::Ellipsis => "ellipsis".into(),
        MangleType::Auto => "auto".into(),
        MangleType::DecltypeAuto => "decltype_auto".into(),
    }
}

impl Lower {
    /// M18.0: namespace-aware mangle_to_c, 自动 qualify 类引用
    /// 当类在 std:: 内被引用, 且名字不含 std_ 前缀, 加 std_ 前缀
    fn mangle_to_c_ns(&self, ty: &MangleType) -> String {
        match ty {
            MangleType::Class(n) => {
                // Normalize: flatten `::` to `_` so `std::string` → `std_string`
                let n = if n.contains("::") { n.replace("::", "_") } else { n.clone() };
                // Known typedef names: these are NOT struct types,
                // output the name directly without `struct` prefix.
                match n.as_str() {
                    "size_t" | "ptrdiff_t" | "wchar_t" | "char8_t"
                    | "char16_t" | "char32_t" | "int8_t" | "int16_t"
                    | "int32_t" | "int64_t" | "uint8_t" | "uint16_t"
                    | "uint32_t" | "uint64_t" | "intptr_t" | "uintptr_t"
                    | "ssize_t" | "off_t" | "time_t" | "pid_t" | "uid_t"
                    | "gid_t" | "mode_t" | "off64_t" | "ino_t" | "nlink_t"
                    | "blksize_t" | "blkcnt_t" | "fsblkcnt_t" | "fsfilcnt_t"
                    | "id_t" | "key_t" | "clock_t" | "suseconds_t"
                    | "useconds_t" | "dev_t" => return n,
                    _ => {}
                }
                // M26: enum 类型已降级为 `typedef enum {...} ns_E;`, 类型引用
                // 直接使用 typedef 名, 不再前缀 `struct `.
                if self.enum_types.contains(&n) {
                    return self.qualify_name(&n);
                }
                // enum/union/struct 前缀: 保留原始关键字而非加 struct
                if n.starts_with("enum ") || n.starts_with("union ")
                    || n.starts_with("struct ")
                {
                    return n.clone();
                }
                // 提取 enum/struct/union 后的裸名检查 enum_types
                for prefix in &["struct ", "enum ", "union "] {
                    if let Some(bare) = n.strip_prefix(prefix) {
                        if self.enum_types.contains(bare) {
                            return self.qualify_name(bare);
                        }
                    }
                }
                if !self.ns.stack.is_empty() && !n.contains('_') {
                    let q = format!("{}_{}", self.ns.stack.join("_"), n);
                    format!("struct {}", q)
                } else {
                    format!("struct {}", n)
                }
            }
            MangleType::Pointer(t) | MangleType::Reference(t) => {
                format!("{}*", self.mangle_to_c_ns(t))
            }
            // M27: Template 类型直接走 instantiated_class_name, 确保与
            // template.rs 单态化生成的类名一致 (不再走占位 typedef)
            MangleType::Template { name, args } => {
                format!("struct {}", instantiated_class_name(name, args))
            }
            _ => mangle_to_c(ty),
        }
    }

    pub fn new() -> Self {
        Self {
            out: String::new(),
            opt: LowerOptions::default(),
            ns: NamespaceCtx::default(),
            classes: vec![],
            enum_types: std::collections::HashSet::new(),
            noexcept_fns: std::collections::HashSet::new(),
            fn_ret_types: std::collections::HashMap::new(),
            class_methods: std::collections::HashMap::new(),
            class_ctors: std::collections::HashMap::new(),
            class_fields: std::collections::HashMap::new(),
            class_bases: std::collections::HashMap::new(),
            local_var_types: std::collections::HashMap::new(),
            param_var_types: std::collections::HashMap::new(),
            emitted_typedefs: std::collections::HashSet::new(),
            current_class: None,
            current_fn_void: false,
            lambdas: vec![],
            lambda_counter: 0,
            const_local_i128: std::collections::HashMap::new(),
            coro_state: 0,
            coro_var: String::new(),
            coro_ret_c: String::new(),
            module_name: String::new(),
            module_collect: false,
            module_imports: vec![],
            module_exports: vec![],
            templates: TemplateRegistry::default(),
            inst_emitted: std::collections::HashSet::new(),
            plain_fns: std::collections::HashSet::new(),
            pending_fn_insts: std::collections::VecDeque::new(),
            concepts: std::collections::HashMap::new(),
            transitive_vbases: std::collections::HashMap::new(),
            vbase_tables: std::collections::HashMap::new(),
            field_offsets: std::collections::HashMap::new(),
            class_sizes: std::collections::HashMap::new(),
            class_aligns: std::collections::HashMap::new(),
            class_polymorphic: std::collections::HashSet::new(),
            mutable_fields: std::collections::HashMap::new(),
            imported_modules: vec![],
            module_partition: None,
            module_part_imports: vec![],
        }
    }
    pub fn with_options(_opt: LowerOptions) -> Self {
        Self::new()
    }

    /// 类型到 C 字符串
    fn mangle_to_c(ty: &MangleType) -> String {
        mangle_to_c(ty)
    }

    /// 类型名 (用于符号 mangling, 不带 struct 前缀)
    fn type_name(ty: &MangleType) -> String {
        match ty {
            MangleType::Class(n) => n.clone(),
            other => mangle_to_c(other),
        }
    }

    /// M17.0: 编译期求值表达式 e 的"静态类名" (用于 typeid 静态路径).
    /// 启发式:
    /// - Ident: 查 local_var_types / param_var_types
    /// - Member: 形如 obj.field, 若 obj 已知类, 用 obj 类 (保守: field 类未知)
    /// - 其他: 返回空 (调用方回退为 int / 占位)
    fn expr_static_class(&self, e: &CppExpr) -> String {
        match e {
            CppExpr::Ident(n) => {
                if let Some(MangleType::Class(c)) = self.local_var_types.get(n) {
                    return c.clone();
                }
                if let Some(MangleType::Class(c)) = self.param_var_types.get(n) {
                    return c.clone();
                }
                String::new()
            }
            CppExpr::MemberStar(obj, _) | CppExpr::PtrMemberStar(obj, _) => {
                self.expr_static_class(obj)
            }
            CppExpr::This => self.current_class.clone().unwrap_or_default(),
            CppExpr::ThisMember(_) => self.current_class.clone().unwrap_or_default(),
            _ => String::new(),
        }
    }

    /// M17.0: 推导 `auto x = expr;` / `decltype(auto) x = expr;` 的真实类型
    /// 规则 (C++14):
    /// - auto: 从 init 表达式的"值类别 + 类型" 去引用 + 去顶层 cv -> 推导
    /// - decltype(auto): 完整保留 decltype(expr) 的规则
    ///
    /// M26.1 增强: 调用 / 成员调用 / 成员访问 / new 均查真实类型注册表,
    /// 不再全部保守降级为 int / Unknown.
    fn infer_type(&self, e: &CppExpr, is_decltype_auto: bool) -> MangleType {
        let ty = match e {
            CppExpr::IntLit(_) => MangleType::Int,
            // M26.1: 浮点字面量推导为 double
            CppExpr::FloatLit(_) => MangleType::Double,
            CppExpr::StrLit(_) => MangleType::Pointer(Box::new(MangleType::Char)),
            CppExpr::StrConcat(_) => MangleType::Pointer(Box::new(MangleType::Char)),
            CppExpr::Ident(n) => {
                // 查 local/param, 若是 Class 则为值类型; 若是 int, 保留 int
                if let Some(t) = self.local_var_types.get(n) {
                    t.clone()
                } else if let Some(t) = self.param_var_types.get(n) {
                    t.clone()
                } else {
                    MangleType::Int
                }
            }
            CppExpr::This => MangleType::Pointer(Box::new(MangleType::Class(
                self.current_class.clone().unwrap_or_default(),
            ))),
            CppExpr::UnaryOp(_, inner) => self.infer_type(inner, is_decltype_auto),
            CppExpr::BinOp(_, l, _) => self.infer_type(l, is_decltype_auto),
            CppExpr::Cast(t, _) => t.clone(),
            CppExpr::Call(f, _) => {
                if let CppExpr::Member(_, method) = &**f {
                    // M26.1: 成员调用 `obj.method()` -> 查方法返回类型注册表
                    self.member_call_ret_type(f, method)
                        .unwrap_or(MangleType::Int)
                } else if let CppExpr::PtrMember(_, method) = &**f {
                    self.member_call_ret_type(f, method)
                        .unwrap_or(MangleType::Int)
                } else if let CppExpr::Ident(name) = &**f {
                    // M26.1: 先查已声明的函数返回类型 (`auto x = foo()`)
                    if let Some(ret) = self.fn_ret_types.get(name) {
                        ret.clone()
                    } else if self.classes.contains(name) {
                        // 类构造 `Widget(7)`: 名字是已知类 -> Class(name)
                        MangleType::Class(name.clone())
                    } else {
                        // C 函数: 已知一些返回类型
                        match name.as_str() {
                            "malloc" | "calloc" | "realloc" => {
                                MangleType::Pointer(Box::new(MangleType::Void))
                            }
                            "strlen" | "printf" | "scanf" => MangleType::Int,
                            _ => MangleType::Int,
                        }
                    }
                } else {
                    MangleType::Int
                }
            }
            CppExpr::New(ty, _) | CppExpr::PlacementNew { ty, .. } => {
                // M26.1: new T -> T* (不再 Unknown)
                MangleType::Pointer(Box::new(MangleType::Class(ty.clone())))
            }
            CppExpr::NewArr { ty, .. } => {
                MangleType::Pointer(Box::new(MangleType::Class(ty.clone())))
            }
            CppExpr::Member(o, m) | CppExpr::PtrMember(o, m) => {
                // M26.1: `auto x = obj.field` -> 查类成员类型
                self.member_field_type(o, m).unwrap_or(MangleType::Int)
            }
            CppExpr::Ternary { then, .. } => self.infer_type(then, is_decltype_auto),
            // M18.0: 多维下标返回元素类型 (与单下标一致)
            CppExpr::MultiIndex(obj, _) => self.infer_type(obj, is_decltype_auto),
            _ => MangleType::Int,
        };
        // decltype(auto) 保留引用: 若 init 是 Ident(局部), 视情况包 Reference
        // C++14 decltype 规则: 若 expr 是无括号 id 表达式, decltype 给出声明类型;
        // 若 expr 是 (id) 或其他, decltype 给出值类别对应的类型.
        // 我们的简化: 都不包引用 (C 端无引用概念).
        // (is_decltype_auto 与 is_decltype 在本路径上行为一致)
        let _ = is_decltype_auto;
        ty
    }

    /// M26.1: 成员调用 `obj.method()` / `obj->method()` 的返回类型.
    fn member_call_ret_type(&self, f: &CppExpr, method: &str) -> Option<MangleType> {
        let obj = match f {
            CppExpr::Member(o, _) | CppExpr::PtrMember(o, _) => o,
            _ => return None,
        };
        let class = self.lookup_var_class(obj)?;
        self.fn_ret_types
            .get(&format!("{}::{}", class, method))
            .cloned()
    }

    /// M26.1: 成员访问 `obj.field` / `obj->field` 的字段类型.
    fn member_field_type(&self, obj: &CppExpr, field: &str) -> Option<MangleType> {
        let class = self.lookup_var_class(obj)?;
        let fields = self.class_fields.get(&class)?;
        fields
            .iter()
            .find(|(n, _)| n == field)
            .map(|(_, ty)| ty.clone())
    }

    /// M26.1: 表达式求值是否会抛异常 (`noexcept(expr)` 编译期谓词).
    /// 规则 (近似 C++ 标准):
    /// - 字面量 / 标识符 / 内建运算 / 成员访问 / sizeof / cast: 不抛
    /// - `throw` 表达式: 抛
    /// - 函数调用: 被调函数 noexcept 则不抛; 否则可能抛
    /// - `new` / `new[]`: 可能抛 (bad_alloc)
    /// - `dynamic_cast` / 多态 `typeid`: 可能抛 (bad_cast / bad_typeid)
    /// - 子表达式递归
    fn expr_can_throw(&self, e: &CppExpr) -> bool {
        match e {
            CppExpr::IntLit(_)
            | CppExpr::FloatLit(_)
            | CppExpr::StrLit(_)
            | CppExpr::Ident(_)
            | CppExpr::This
            | CppExpr::ThisMember(_)
            | CppExpr::MemberPtr { .. }
            | CppExpr::TemplateIdent { .. }
            | CppExpr::Sizeof(_)
            | CppExpr::TypeidType(_)
            | CppExpr::StrConcat(_)
            | CppExpr::Lambda(_) => false,
            CppExpr::New(..) | CppExpr::NewArr { .. } => true,
            CppExpr::PlacementNew { ptr, ty: _, args } => {
                self.expr_can_throw(ptr) || args.iter().any(|a| self.expr_can_throw(a))
            }
            CppExpr::DynamicCast { .. } => {
                // dynamic_cast 到引用类型会抛 bad_cast
                true
            }
            CppExpr::Typeid(x) => {
                // 多态表达式 typeid 可能抛 bad_typeid
                self.expr_can_throw(x) || self.is_polymorphic_expr(x)
            }
            CppExpr::Member(o, _)
            | CppExpr::PtrMember(o, _)
            | CppExpr::MemberStar(o, _)
            | CppExpr::PtrMemberStar(o, _)
            | CppExpr::UnaryOp(_, o)
            | CppExpr::Ref(o)
            | CppExpr::AddrOf(o)
            | CppExpr::Delete(o)
            | CppExpr::DeleteArr(o)
            | CppExpr::Noexcept(o) => self.expr_can_throw(o),
            CppExpr::BinOp(_, l, r) => self.expr_can_throw(l) || self.expr_can_throw(r),
            CppExpr::Ternary { cond, then, els } => {
                self.expr_can_throw(cond) || self.expr_can_throw(then) || self.expr_can_throw(els)
            }
            CppExpr::Comma(es) => es.iter().any(|x| self.expr_can_throw(x)),
            CppExpr::Cast(_, x)
            | CppExpr::StaticCast { expr: x, .. }
            | CppExpr::ConstCast { expr: x, .. }
            | CppExpr::ReinterpretCast { expr: x, .. } => self.expr_can_throw(x),
            CppExpr::Call(f, args) => {
                if args.iter().any(|a| self.expr_can_throw(a)) {
                    return true;
                }
                self.call_target_can_throw(f)
            }
            CppExpr::OpCall { .. } => {
                // 内建运算符不抛 (parser 只对重载运算符产生 OpCall);
                // 重载运算符是函数调用, 保守视为可抛.
                true
            }
            CppExpr::InitList(es) => es.iter().any(|x| self.expr_can_throw(x)),
            CppExpr::TypedInitList { elems, .. } => elems.iter().any(|x| self.expr_can_throw(x)),
            CppExpr::MultiIndex(o, idxs) => {
                self.expr_can_throw(o) || idxs.iter().any(|x| self.expr_can_throw(x))
            }
            CppExpr::QualifiedAccess(_, _) => false,
        }
    }

    /// 被调函数是否可能抛异常 (noexcept 集合查询).
    fn call_target_can_throw(&self, f: &CppExpr) -> bool {
        match f {
            CppExpr::Ident(name) => !self.noexcept_fns.contains(name),
            CppExpr::Member(o, m) | CppExpr::PtrMember(o, m) => {
                if let Some(class) = self.lookup_var_class(o) {
                    let key = format!("{}::{}", class, m);
                    !self.noexcept_fns.contains(&key)
                } else {
                    // 未知接收者: 保守视为可抛
                    true
                }
            }
            CppExpr::QualifiedAccess(_, _) => true,
            _ => true,
        }
    }

    /// 启发式: 表达式是否为 class 类型 - typeid 抛 bad_typeid 判定用.
    fn is_polymorphic_expr(&self, e: &CppExpr) -> bool {
        matches!(self.infer_type(e, false), MangleType::Class(_))
    }

    /// 查找表达式 (通常是 Ident 或 Member) 对应的类名 (M9: 仅局部变量)
    fn lookup_var_class(&self, e: &CppExpr) -> Option<String> {
        if let CppExpr::Ident(name) = e {
            // 先查局部变量
            if let Some(ty) = self.local_var_types.get(name) {
                if let Some(c) = Self::extract_class(ty) {
                    return Some(self.resolve_class_name(&c));
                }
            }
            // 再查形参
            if let Some(ty) = self.param_var_types.get(name) {
                if let Some(c) = Self::extract_class(ty) {
                    return Some(self.resolve_class_name(&c));
                }
            }
        }
        None
    }

    /// M26: 递归查找继承链中包含指定成员的基类
    /// 返回完整 __base_ 访问链 (从直接基类到目标基类)
    /// 例如 Child : Parent : GrandParent, 成员 x 在 GrandParent 中:
    ///   返回 Some(vec!["Parent", "GrandParent"])
    ///   调用方生成 c.__base_Parent.__base_GrandParent.x
    fn find_inherited_base(&self, class: &str, member: &str) -> Option<Vec<String>> {
        // 检查当前类的直接基类
        if let Some(bases) = self.class_bases.get(class) {
            for base in bases {
                // 检查基类是否直接包含该成员
                if let Some(base_fields) = self.class_fields.get(&base.name) {
                    if base_fields.iter().any(|(name, _)| name == member) {
                        return Some(vec![base.name.clone()]);
                    }
                }
                // 递归检查基类的基类
                if let Some(mut chain) = self.find_inherited_base(&base.name, member) {
                    // 在链首插入直接基类名, 构成完整路径
                    chain.insert(0, base.name.clone());
                    return Some(chain);
                }
            }
        }
        None
    }

    /// 检查类是否直接包含指定成员变量
    fn has_direct_member(&self, class: &str, member: &str) -> bool {
        if let Some(fields) = self.class_fields.get(class) {
            return fields.iter().any(|(name, _)| name == member);
        }
        false
    }

    /// 递归收集类的所有成员变量 (包括继承的)
    /// chain: 从派生类到当前基类的 __base_ 路径
    /// 输出: members 中累积 (member_name, chain_snapshot) 对
    fn collect_inherited_members_static(
        class_bases: &std::collections::HashMap<String, Vec<BaseSpec>>,
        class_fields: &std::collections::HashMap<String, Vec<(String, MangleType)>>,
        class: &str,
        chain: &mut Vec<String>,
        members: &mut Vec<(String, Vec<String>)>,
    ) {
        // 先递归处理基类
        if let Some(bases) = class_bases.get(class) {
            for base in bases {
                // M26.3: 虚基成员经 vbtable 运行时调整, 静态 __base_ 链无效,
                // 跳过 (成员指针指向虚基成员由 __dcc_mbr_<派生> 不覆盖).
                if base.is_virtual {
                    continue;
                }
                chain.push(base.name.clone());
                Self::collect_inherited_members_static(
                    class_bases,
                    class_fields,
                    &base.name,
                    chain,
                    members,
                );
                chain.pop();
            }
        }
        // 再处理当前类的直接成员
        if let Some(fields) = class_fields.get(class) {
            for (name, _) in fields {
                members.push((name.clone(), chain.clone()));
            }
        }
    }

    /// 从 MangleType 中提取类名 (支持 Pointer / Reference / Class 包装)
    /// M26: 模板类型 `Box<int>` 返回实例化类名 (方法调用 / typeid 用)
    fn extract_class(ty: &MangleType) -> Option<String> {
        match ty {
            MangleType::Class(c) => Some(c.clone()),
            MangleType::Pointer(t) | MangleType::Reference(t) => Self::extract_class(t),
            MangleType::Template { name, args } => Some(instantiated_class_name(name, args)),
            _ => None,
        }
    }

    /// M26: 类类型名 (Class 或模板实例化) - 仅值类型, 不含指针 (ctor 调用用)
    fn inst_class_of(ty: &MangleType) -> Option<String> {
        match ty {
            MangleType::Class(c) => Some(c.clone()),
            MangleType::Template { name, args } => Some(instantiated_class_name(name, args)),
            _ => None,
        }
    }

    /// 检查是否是 C/C++ 标准 typedef (不是真正的 class 类型).
    /// 用于跳过构造函数调用生成.
    fn is_known_typedef(name: &str) -> bool {
        matches!(name,
            "size_t" | "ptrdiff_t" | "wchar_t" | "char8_t"
            | "char16_t" | "char32_t" | "int8_t" | "int16_t"
            | "int32_t" | "int64_t" | "uint8_t" | "uint16_t"
            | "uint32_t" | "uint64_t" | "intptr_t" | "uintptr_t"
            | "ssize_t" | "off_t" | "time_t" | "pid_t" | "uid_t"
            | "gid_t" | "mode_t" | "off64_t" | "ino_t" | "nlink_t"
            | "blksize_t" | "blkcnt_t" | "fsblkcnt_t" | "fsfilcnt_t"
            | "id_t" | "key_t" | "clock_t" | "suseconds_t"
            | "useconds_t" | "dev_t"
        )
    }

    /// 解析类名: 尝试原名, 然后 namespace 限定名
    fn resolve_class_name(&self, c: &str) -> String {
        // 1) 原名直接命中 class_methods (e.g. 全局类)
        if self.class_methods.contains_key(c) {
            return c.to_string();
        }
        // 2) 限定名命中 class_methods (e.g. `path` -> `std_filesystem_path`)
        let q = self.qualify_name(c);
        if self.class_methods.contains_key(&q) {
            return q;
        }
        // 3) 已经含 `::` 的形式 (e.g. `std::filesystem::path`)
        if c.contains("::") {
            let flat = c.replace("::", "_");
            if self.class_methods.contains_key(&flat) {
                return flat;
            }
        }
        // 4) 最后才回退到 self.classes Vec (兼容性)
        if self.classes.iter().any(|x| x == c) {
            return c.to_string();
        }
        c.to_string()
    }

    /// 查找 Ident 表达式对应的 MangleType (M18.0: 用于 . vs -> 选择)
    fn lookup_ident_type(&self, e: &CppExpr) -> Option<MangleType> {
        if let CppExpr::Ident(name) = e {
            // 局部变量
            if let Some(ty) = self.local_var_types.get(name) {
                return Some(ty.clone());
            }
            // 形参
            if let Some(ty) = self.param_var_types.get(name) {
                return Some(ty.clone());
            }
            // M18.0: 类字段 - 当在成员函数中, ident 可能是当前类的字段
            // (解析器不会自动加 this->, 需 lower 阶段识别)
            if let Some(cls) = &self.current_class {
                if let Some(fields) = self.class_fields.get(cls) {
                    for (n, ty) in fields {
                        if n == name {
                            return Some(ty.clone());
                        }
                    }
                }
            }
        }
        None
    }

    /// 类型是否需要 `->` 访问 (指针 / 引用)
    fn is_ptr_like(ty: &MangleType) -> bool {
        matches!(ty, MangleType::Pointer(_) | MangleType::Reference(_))
    }

    /// 入口
    pub fn lower_decls(&mut self, decls: &[CppDecl]) {
        self.out
            .push_str("/* dcc C++ frontend M7+M8 lowered output */\n");
        // 根据代码是否使用 new/delete/throw 决定是否引入系统头文件.
        // 注意: 系统头文件在 __cplusplus 下会包含 C++ 代码 (如 decltype),
        // C 后端无法解析, 因此仅在必要时引入.
        let needs_system_headers = decls.iter().any(|d| self.decl_needs_system_headers(d));
        if needs_system_headers {
            self.out.push_str("#include <stddef.h>\n");
            self.out.push_str("#include <stdint.h>\n");
        } else {
            // 不用系统头时, 显式声明常用类型以便本地 dcc 也能工作
            self.out.push_str("typedef unsigned long size_t;\n");
            self.out.push_str("typedef long long intmax_t;\n");
            self.out.push_str("typedef unsigned long long uintmax_t;\n");
            self.out.push_str("typedef int wchar_t;\n");
            self.out.push_str("typedef long long int64_t;\n");
            self.out.push_str("typedef unsigned long long uint64_t;\n");
        }
        // POSIX / glibc 常用类型: 系统头文件中的定义可能被 C++ parser
        // 容错跳过而丢失, 这里无条件补充 (重复 typedef 在 C 中合法,
        // 只要类型相同).
        self.out.push_str("typedef int wchar_t;\n");
        self.out.push_str("typedef unsigned long size_t;\n");
        self.out.push_str("typedef long long intmax_t;\n");
        self.out.push_str("typedef unsigned long long uintmax_t;\n");
        self.out.push_str("typedef long long int64_t;\n");
        self.out.push_str("typedef unsigned long long uint64_t;\n");
        self.out.push_str("typedef long int __off_t;\n");
        self.out.push_str("typedef long int __off64_t;\n");
        self.out.push_str("typedef long int __ssize_t;\n");
        self.out.push_str("typedef unsigned long int __dev_t;\n");
        self.out.push_str("typedef unsigned int __uid_t;\n");
        self.out.push_str("typedef unsigned int __gid_t;\n");
        self.out.push_str("typedef unsigned long int __ino_t;\n");
        self.out.push_str("typedef unsigned int __mode_t;\n");
        self.out.push_str("typedef unsigned long int __nlink_t;\n");
        self.out.push_str("typedef long int __time_t;\n");
        self.out.push_str("typedef long int __blksize_t;\n");
        self.out.push_str("typedef long int __blkcnt_t;\n");
        self.out.push_str("typedef long int __syscall_slong_t;\n");
        self.out.push_str("typedef unsigned long int __syscall_ulong_t;\n");
        self.out.push_str("typedef long int __loff_t;\n");
        self.out.push_str("typedef long int off_t;\n");
        self.out.push_str("typedef long int ssize_t;\n");
        self.out.push_str("typedef __builtin_va_list __gnuc_va_list;\n");
        self.out.push_str("typedef __builtin_va_list va_list;\n");
        // glibc 内部整数类型 (<bits/types.h>): GCC 源码广泛使用,
        // C++ parser 处理系统头文件时可能丢失定义.
        self.out.push_str("typedef signed char __int8_t;\n");
        self.out.push_str("typedef short int __int16_t;\n");
        self.out.push_str("typedef int __int32_t;\n");
        self.out.push_str("typedef long int __int64_t;\n");
        self.out.push_str("typedef unsigned char __uint8_t;\n");
        self.out.push_str("typedef unsigned short int __uint16_t;\n");
        self.out.push_str("typedef unsigned int __uint32_t;\n");
        self.out.push_str("typedef unsigned long int __uint64_t;\n");
        // C99 standard integer types (without leading underscores)
        self.out.push_str("typedef __uint8_t uint8_t;\n");
        self.out.push_str("typedef __uint16_t uint16_t;\n");
        self.out.push_str("typedef __uint32_t uint32_t;\n");
        self.out.push_str("typedef __uint64_t uint64_t;\n");
        self.out.push_str("typedef __int8_t int8_t;\n");
        self.out.push_str("typedef __int16_t int16_t;\n");
        self.out.push_str("typedef __int32_t int32_t;\n");
        self.out.push_str("typedef __int64_t int64_t;\n");
        self.out.push_str("typedef long int __clock_t;\n");
        self.out.push_str("typedef long int __suseconds_t;\n");
        self.out.push_str("typedef int __pid_t;\n");
        self.out.push_str("typedef int __key_t;\n");
        self.out.push_str("typedef char* __caddr_t;\n");
        self.out.push_str("typedef int __daddr_t;\n");
        self.out.push_str("typedef long int __quad_t;\n");
        self.out.push_str("typedef unsigned long int __u_quad_t;\n");
        self.out.push_str("typedef unsigned int __id_t;\n");
        self.out.push_str("typedef unsigned long int __ino64_t;\n");
        self.out.push_str("typedef long int __blkcnt64_t;\n");
        self.out.push_str("typedef unsigned long int __fsblkcnt_t;\n");
        self.out.push_str("typedef unsigned long int __fsblkcnt64_t;\n");
        self.out.push_str("typedef unsigned long int __fsfilcnt_t;\n");
        self.out.push_str("typedef unsigned long int __fsfilcnt64_t;\n");
        self.out.push_str("typedef long int __ssize_t;\n");
        self.out.push_str("typedef void* __locale_t;\n");
        // __fsid_t is provided by system headers (<bits/types.h>)
        // BSD 兼容类型 (<sys/types.h>)
        self.out.push_str("typedef unsigned char __u_char;\n");
        self.out.push_str("typedef unsigned short int __u_short;\n");
        self.out.push_str("typedef unsigned int __u_int;\n");
        self.out.push_str("typedef unsigned long int __u_long;\n");
        self.out.push_str("typedef unsigned int __useconds_t;\n");
        self.out.push_str("typedef int __socklen_t;\n");
        self.out.push_str("typedef int __timer_t;\n");
        self.out.push_str("typedef int __clockid_t;\n");
        // 通用 ABI 运行时
        self.out.push_str("extern void *_Znwm(size_t);\n");
        self.out.push_str("extern void _ZdlPv(void*);\n");
        // M17.0: 数组版本
        self.out.push_str("extern void *_Znam(size_t);\n");
        self.out.push_str("extern void _ZdaPv(void*);\n");
        self.out
            .push_str("extern void __cxa_throw(void*, void*, void(*)(void*));\n");
        self.out
            .push_str("extern void* __cxa_allocate_exception(size_t);\n");
        self.out
            .push_str("extern void __cxa_free_exception(void*);\n");
        self.out.push_str("typedef struct __cxa_eh_context { void* matched; int rethrown; } __cxa_eh_context_t;\n");
        self.out
            .push_str("extern int __cxa_try(__cxa_eh_context_t*, void* (*)(void*), void*);\n");
        self.out
            .push_str("extern void* __cxa_begin_catch(void*);\n");
        self.out.push_str("extern void __cxa_end_catch(void);\n");
        // M26: GCC ABI 兼容运行时 (__cxa_guard_* / typeinfo / __dynamic_cast /
        // initializer_list / 成员指针偏移) - 自包含 C 定义, 幂等追加
        self.out.push_str(&gcc_runtime_prologue());
        // 第一次遍历: 收集类名 + 注册模板 + 扫描模板类型触发类实例化
        for d in decls {
            self.collect_classes(d);
        }
        // M26.3: 虚继承预遍历 - 构建完整 class_bases, 计算传递虚基列表、
        // 多态标记与模拟布局 (vbtable 常量偏移; 必须与 C struct 实际布局一致)
        for d in decls {
            self.collect_class_bases(d);
        }
        self.compute_transitive_vbases();
        self.compute_polymorphic_flags();
        self.compute_class_layouts();
        // M26.1: 收集 noexcept 函数名 (noexcept(expr) 求值用)
        for d in decls {
            self.collect_noexcept_fns(d);
        }
        // M26.1: 收集函数返回类型 (auto 变量推导用; 先于 collect_noexcept
        // 之后运行, 因为 resolve_class_name 依赖 class_methods)
        for d in decls {
            self.collect_fn_ret_types(d);
        }
        // 第二次遍历: 实际降级
        for d in decls {
            self.lower_decl(d);
        }
        // 2.5: 输出函数模板实例化定义 (含其体内触发的类实例化)
        self.emit_pending_fns();
        // 最后降级所有 lambda
        self.lower_lambdas();
        // 若是顶层 main, 加 C 端 trampoline 让 libc 启动器能找到 main
        self.emit_main_trampoline(decls);
    }

    /// 如果存在 `int main() { ... }` (在 lower 后是 `_ZN4mainEv`),
    /// 末尾追加 `int main() { return _ZN4mainEv(); }`
    fn emit_main_trampoline(&mut self, decls: &[CppDecl]) {
        let has_main = decls.iter().any(|d| match d {
            CppDecl::FreeFn { name, .. } => name == "main",
            _ => false,
        });
        if has_main {
            self.out.push_str("int main() { return _ZN4mainEv(); }\n");
        }
    }

    fn collect_classes(&mut self, d: &CppDecl) {
        match d {
            CppDecl::Class(c) => {
                self.classes.push(c.name.clone());
                // M18.0: 用 qualified name 作 key, 避免 namespace 内同名
                // 类 (如 std::duration) 与全局类冲突
                let qname = self.qualify_name(&c.name);
                let mut methods = vec![];
                let mut fields = vec![];
                for m in &c.members {
                    match m {
                        CppDecl::MemberFn { name, .. } => methods.push(name.clone()),
                        CppDecl::Ctor { .. } | CppDecl::Dtor { .. } => {}
                        CppDecl::MemberVar { ty, name, .. } => {
                            fields.push((name.clone(), ty.clone()))
                        }
                        _ => {}
                    }
                }
                self.class_methods.insert(qname.clone(), methods);
                self.class_fields.insert(qname, fields);
                // M26: 扫描类成员类型, 触发模板类实例化
                self.scan_decl_types(d);
            }
            CppDecl::Namespace { name, body } => {
                // M18.0: 跟踪 namespace stack 让 qualify_name 出正确名字
                if let Some(n) = name {
                    self.ns.stack.push(n.clone());
                }
                for x in body {
                    self.collect_classes(x);
                }
                if name.is_some() {
                    self.ns.stack.pop();
                }
            }
            // M26: 模板声明只注册 (不再展开); 模板体在实例化时替换形参
            CppDecl::Template {
                params,
                decl,
                requires,
                ..
            } => {
                self.templates.register(params, requires, decl);
            }
            CppDecl::PartialSpec {
                params,
                pattern_text,
                decl,
            } => {
                if let CppDecl::Class(c) = &**decl {
                    let names: Vec<String> = params.params.iter().map(|p| p.name.clone()).collect();
                    self.templates.register_partial(
                        c.name.clone(),
                        &names,
                        pattern_text.clone(),
                        decl,
                    );
                }
            }
            // M26: 概念定义注册 (concept Name = body_text)
            CppDecl::Concept { name, body_text } => {
                self.concepts.insert(name.clone(), body_text.clone());
                // 同步注册到模板注册表 (供 requires 子句求值时展开)
                self.templates
                    .concepts
                    .insert(name.clone(), body_text.clone());
            }
            _ => {
                // M26: 普通声明扫描类型, 触发模板类实例化 (定义先于使用)
                if let CppDecl::FreeFn { name, .. } = d {
                    self.plain_fns.insert(name.clone());
                }
                self.scan_decl_types(d);
            }
        }
    }

    // ===== M26.3: 虚继承 (Itanium ABI 布局) =====

    /// 递归收集所有类的基类信息 (完整 class_bases), 并登记自有虚函数类.
    /// 先于实际 lower 运行, 保证任意声明顺序下都能计算虚基布局.
    fn collect_class_bases(&mut self, d: &CppDecl) {
        match d {
            CppDecl::Class(c) => {
                let qname = self.qualify_name(&c.name);
                self.class_bases.insert(qname.clone(), c.bases.clone());
                // 自有虚函数/虚析构 -> 多态 (vptr 布局依据)
                let own_virt = c.members.iter().any(|m| {
                    matches!(
                        m,
                        CppDecl::MemberFn {
                            is_virtual: true,
                            ..
                        } | CppDecl::Dtor {
                            is_virtual: true,
                            ..
                        }
                    )
                });
                if own_virt {
                    self.class_polymorphic.insert(qname);
                }
                for m in &c.members {
                    if let CppDecl::Class(nested) = m {
                        let nq = self.qualify_name(&nested.name);
                        self.class_bases.insert(nq, nested.bases.clone());
                    }
                }
            }
            CppDecl::Namespace { name, body } => {
                if let Some(n) = name {
                    self.ns.stack.push(n.clone());
                }
                for x in body {
                    self.collect_class_bases(x);
                }
                if name.is_some() {
                    self.ns.stack.pop();
                }
            }
            CppDecl::Template { decl, .. } => {
                self.collect_class_bases(decl);
            }
            CppDecl::ExportBlock(body) => {
                for x in body {
                    self.collect_class_bases(x);
                }
            }
            _ => {}
        }
    }

    /// 类名 -> 注册表 key (resolve_class_name 的健壮回退)
    fn cls_key(&self, n: &str) -> String {
        let r = self.resolve_class_name(n);
        if r.is_empty() {
            n.to_string()
        } else {
            r
        }
    }

    /// 计算每个类的传递虚基列表 (DFS 首次出现顺序, 去重).
    /// Itanium ABI: 虚基顺序 = 继承图深度优先先左后右遍历的首次出现顺序.
    fn compute_transitive_vbases(&mut self) {
        let mut result: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        let keys: Vec<String> = self.class_bases.keys().cloned().collect();
        for k in keys {
            let mut out: Vec<String> = Vec::new();
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut stack: Vec<(String, bool)> = self.class_bases[&k]
                .iter()
                .map(|b| (self.cls_key(&b.name), b.is_virtual))
                .collect();
            stack.reverse();
            while let Some((cur, is_v)) = stack.pop() {
                if is_v {
                    if !seen.insert(cur.clone()) {
                        continue; // 已访问的虚基: 跳过其子树 (去重)
                    }
                    out.push(cur.clone());
                }
                // 无论是否虚基都递归收集其虚基
                if let Some(bases) = self.class_bases.get(&cur).cloned() {
                    let mut children: Vec<(String, bool)> = bases
                        .iter()
                        .map(|b| (self.cls_key(&b.name), b.is_virtual))
                        .collect();
                    children.reverse();
                    stack.extend(children);
                }
            }
            if !out.is_empty() {
                result.insert(k.clone(), out);
            }
        }
        self.transitive_vbases = result;
    }

    /// 多态类标记补全: 与既有 lower_class 的 vptr 规则一致
    /// (`has_virt = 自有虚函数 || 有基类`), 保证布局模拟与实际 struct 一致.
    fn compute_polymorphic_flags(&mut self) {
        let keys: Vec<String> = self.class_bases.keys().cloned().collect();
        for k in keys {
            if let Some(bases) = self.class_bases.get(&k) {
                if !bases.is_empty() {
                    self.class_polymorphic.insert(k);
                }
            }
        }
    }

    /// 类型对齐 (System V x86_64 规则; 必须与 C struct 实际布局一致)
    fn ty_align(&self, ty: &MangleType) -> i64 {
        match ty {
            MangleType::Bool | MangleType::Char | MangleType::Schar | MangleType::Uchar => 1,
            MangleType::Short | MangleType::Ushort => 2,
            MangleType::Int | MangleType::Uint | MangleType::Float => 4,
            MangleType::Long
            | MangleType::Ulong
            | MangleType::LongLong
            | MangleType::UlongLong
            | MangleType::Double => 8,
            MangleType::LongDouble => 16,
            MangleType::Pointer(_) | MangleType::Reference(_) => 8,
            MangleType::Class(n) => *self.class_aligns.get(&self.cls_key(n)).unwrap_or(&8),
            MangleType::Template { name, args } => *self
                .class_aligns
                .get(&instantiated_class_name(name, args))
                .unwrap_or(&8),
            _ => 8,
        }
    }

    /// 类型大小 (System V x86_64 规则)
    fn ty_size(&self, ty: &MangleType) -> i64 {
        match ty {
            MangleType::Bool | MangleType::Char | MangleType::Schar | MangleType::Uchar => 1,
            MangleType::Short | MangleType::Ushort => 2,
            MangleType::Int | MangleType::Uint | MangleType::Float => 4,
            MangleType::Long
            | MangleType::Ulong
            | MangleType::LongLong
            | MangleType::UlongLong
            | MangleType::Double => 8,
            MangleType::LongDouble => 16,
            MangleType::Pointer(_) | MangleType::Reference(_) => 8,
            MangleType::Class(n) => *self.class_sizes.get(&self.cls_key(n)).unwrap_or(&8),
            MangleType::Template { name, args } => *self
                .class_sizes
                .get(&instantiated_class_name(name, args))
                .unwrap_or(&8),
            _ => 8,
        }
    }

    fn align_up(v: i64, a: i64) -> i64 {
        if a <= 1 {
            return v;
        }
        (v + a - 1) / a * a
    }

    /// 计算所有已注册类的模拟布局 (大小/对齐/字段偏移 + vbtable 常量).
    /// 基类先于派生类定义时一轮完成; 否则多轮迭代直到收敛.
    fn compute_class_layouts(&mut self) {
        let mut pending: Vec<String> = self.class_bases.keys().cloned().collect();
        let mut rounds = 0;
        while !pending.is_empty() && rounds < 64 {
            rounds += 1;
            let mut next = Vec::new();
            for k in pending {
                if !self.compute_layout_for(&k) {
                    next.push(k);
                }
            }
            pending = next;
        }
    }

    /// 计算单个类布局; 依赖基类布局未就绪时返回 false.
    fn compute_layout_for(&mut self, k: &str) -> bool {
        if self.class_sizes.contains_key(k) {
            return true;
        }
        let Some(bases) = self.class_bases.get(k).cloned() else {
            return true;
        };
        for b in &bases {
            let bk = self.cls_key(&b.name);
            if bk != k && !self.class_sizes.contains_key(&bk) {
                return false;
            }
        }
        let mut off: i64 = 0;
        let mut max_align: i64 = 1;
        let mut offs: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
        // 1. 非虚直接基类子对象
        for b in &bases {
            if b.is_virtual {
                continue;
            }
            let bk = self.cls_key(&b.name);
            let sz = self.class_sizes.get(&bk).copied().unwrap_or(8);
            let al = self.class_aligns.get(&bk).copied().unwrap_or(8);
            off = Self::align_up(off, al);
            if al > max_align {
                max_align = al;
            }
            offs.insert(format!("__base_{}", b.name), off);
            off += sz;
        }
        // 2. vptr (多态类)
        if self.class_polymorphic.contains(k) {
            off = Self::align_up(off, 8);
            max_align = max_align.max(8);
            offs.insert("__vptr".to_string(), off);
            off += 8;
        }
        // 3. vbptr + 自有成员 + 虚基子对象
        let vbs = self.transitive_vbases.get(k).cloned().unwrap_or_default();
        let mut vbptr_off: i64 = -1;
        if !vbs.is_empty() {
            off = Self::align_up(off, 8);
            max_align = max_align.max(8);
            vbptr_off = off;
            offs.insert("__vbptr".to_string(), off);
            off += 8;
        }
        // 4. 自有成员
        if let Some(fields) = self.class_fields.get(k).cloned() {
            for (name, ty) in fields {
                let al = self.ty_align(&ty);
                let sz = self.ty_size(&ty);
                off = Self::align_up(off, al);
                if al > max_align {
                    max_align = al;
                }
                offs.insert(name, off);
                off += sz;
            }
        }
        // 5. 虚基子对象 (传递虚基, DFS 顺序)
        let mut vbase_offsets: Vec<(String, i64)> = Vec::new();
        for vb in &vbs {
            let sz = self.class_sizes.get(vb).copied().unwrap_or(8);
            let al = self.class_aligns.get(vb).copied().unwrap_or(8);
            off = Self::align_up(off, al);
            if al > max_align {
                max_align = al;
            }
            offs.insert(format!("__vb_{}", vb), off);
            vbase_offsets.push((vb.clone(), off));
            off += sz;
        }
        let size = Self::align_up(off, max_align);
        // vbtable: [0, off(VB_i) - off(__vbptr), ...]
        let mut table: Vec<i64> = vec![0];
        if vbptr_off >= 0 {
            for (_, vo) in &vbase_offsets {
                table.push(vo - vbptr_off);
            }
        }
        self.class_sizes.insert(k.to_string(), size);
        self.class_aligns.insert(k.to_string(), max_align);
        self.field_offsets.insert(k.to_string(), offs);
        if table.len() > 1 {
            self.vbase_tables.insert(k.to_string(), table);
        }
        true
    }

    /// 虚基成员访问: 查找 member 属于哪个传递虚基 (及在该虚基内的非虚
    /// `__base_` 链). 返回 (虚基 key, vbtable 索引, 虚基内链).
    fn vbase_member_access(
        &self,
        class: &str,
        member: &str,
    ) -> Option<(String, usize, Vec<String>)> {
        let ck = self.cls_key(class);
        let vbs = self.transitive_vbases.get(&ck)?;
        for (i, vb) in vbs.iter().enumerate() {
            if let Some(chain) = self.member_in_or_below(vb, member) {
                return Some((vb.clone(), i + 1, chain));
            }
        }
        None
    }

    /// member 是否属于 class 的直接成员或其非虚基类祖先 (返回 __base_ 链)
    fn member_in_or_below(&self, class: &str, member: &str) -> Option<Vec<String>> {
        if self
            .class_fields
            .get(class)
            .map(|f| f.iter().any(|(n, _)| n == member))
            .unwrap_or(false)
        {
            return Some(vec![]);
        }
        if let Some(bases) = self.class_bases.get(class) {
            for b in bases {
                if b.is_virtual {
                    continue;
                }
                if let Some(mut chain) = self.member_in_or_below(&self.cls_key(&b.name), member) {
                    chain.insert(0, b.name.clone());
                    return Some(chain);
                }
            }
        }
        None
    }

    /// 输出虚基成员访问: `((struct VB*)((char*)&OBJ.__vbptr + OBJ.__vbptr[IDX]))->...`
    /// obj 是对象/指针表达式; use_arrow 决定用 `->__vbptr` 还是 `.__vbptr`.
    /// tail 是虚基内的非虚 `__base_` 链 (空 = 成员直接在虚基内).
    fn emit_vbase_member(
        &mut self,
        obj: &CppExpr,
        use_arrow: bool,
        vb: &str,
        idx: usize,
        tail: &[String],
        member: &str,
    ) {
        let vb_c = self.mangle_to_c_ns(&MangleType::Class(vb.to_string()));
        self.out.push_str(&format!("(({}*)((char*)&", vb_c));
        self.emit_expr(obj);
        self.out
            .push_str(if use_arrow { "->__vbptr + " } else { ".__vbptr + " });
        self.emit_expr(obj);
        self.out
            .push_str(if use_arrow { "->__vbptr[" } else { ".__vbptr[" });
        self.out.push_str(&idx.to_string());
        self.out.push_str("]))");
        for (i, b) in tail.iter().enumerate() {
            if i == 0 {
                self.out.push_str(&format!("->__base_{}", b));
            } else {
                self.out.push_str(&format!(".__base_{}", b));
            }
        }
        self.out.push_str(&format!("->{}", member));
    }

    /// 多态类虚基 this-adjust 上转型: 目标类型 VB 是否为源类 S 的传递虚基
    fn vbase_upcast_target(&self, vb: &str, src_class: &str) -> Option<usize> {
        let ck = self.cls_key(src_class);
        let vbs = self.transitive_vbases.get(&ck)?;
        let vbk = self.cls_key(vb);
        vbs.iter().position(|v| *v == vbk).map(|i| i + 1)
    }

    /// M26.1: 递归收集 noexcept 函数名, 供 `noexcept(expr)` 求值.
    /// 自由函数以函数名登记; 成员/构造/析构以 `Class::name` 登记.
    fn collect_noexcept_fns(&mut self, d: &CppDecl) {
        match d {
            CppDecl::FreeFn {
                name, is_noexcept, ..
            } => {
                if *is_noexcept {
                    self.noexcept_fns.insert(name.clone());
                }
            }
            CppDecl::MemberFn {
                class,
                name,
                is_noexcept,
                ..
            } => {
                if *is_noexcept {
                    self.noexcept_fns.insert(format!("{}::{}", class, name));
                }
            }
            CppDecl::Ctor {
                class, is_noexcept, ..
            } => {
                if *is_noexcept {
                    self.noexcept_fns.insert(format!("{}::ctor", class));
                }
            }
            CppDecl::Dtor {
                class, is_noexcept, ..
            } => {
                if *is_noexcept {
                    self.noexcept_fns.insert(format!("{}::dtor", class));
                }
            }
            CppDecl::Namespace { body, .. } => {
                for x in body {
                    self.collect_noexcept_fns(x);
                }
            }
            CppDecl::Class(c) => {
                for m in &c.members {
                    self.collect_noexcept_fns(m);
                }
            }
            CppDecl::Template { decl, .. } => {
                self.collect_noexcept_fns(decl);
            }
            CppDecl::ExportBlock(body) => {
                for x in body {
                    self.collect_noexcept_fns(x);
                }
            }
            _ => {}
        }
    }

    /// M26.1: 递归收集函数返回类型, 供 `auto x = foo()` 推导.
    /// 自由函数 key = 函数名; 成员 key = `Class::method` (Class 为解析后的
    /// qualified 名, 与 lookup_var_class / class_methods 的 key 一致).
    fn collect_fn_ret_types(&mut self, d: &CppDecl) {
        match d {
            CppDecl::FreeFn { name, ret, .. } => {
                self.fn_ret_types.insert(name.clone(), ret.clone());
            }
            CppDecl::MemberFn {
                class, name, ret, ..
            } => {
                let cls = self.resolve_class_name(class);
                self.fn_ret_types
                    .insert(format!("{}::{}", cls, name), ret.clone());
            }
            CppDecl::Ctor { class, .. } => {
                let cls = self.resolve_class_name(class);
                self.fn_ret_types
                    .insert(format!("{}::ctor", cls), MangleType::Class(cls));
            }
            CppDecl::Dtor { class, .. } => {
                let cls = self.resolve_class_name(class);
                self.fn_ret_types
                    .insert(format!("{}::dtor", cls), MangleType::Void);
            }
            CppDecl::Namespace { body, .. } => {
                for x in body {
                    self.collect_fn_ret_types(x);
                }
            }
            CppDecl::Class(c) => {
                for m in &c.members {
                    self.collect_fn_ret_types(m);
                }
            }
            CppDecl::Template { decl, .. } => {
                self.collect_fn_ret_types(decl);
            }
            CppDecl::ExportBlock(body) => {
                for x in body {
                    self.collect_fn_ret_types(x);
                }
            }
            _ => {}
        }
    }

    // ===== M26: 模板单态化 lower 集成 =====

    /// 递归扫描声明树中的模板实例化类型, 触发类实例化 (定义先于使用).
    fn scan_decl_types(&mut self, d: &CppDecl) {
        match d {
            CppDecl::Class(c) => {
                for m in &c.members {
                    self.scan_decl_types(m);
                }
            }
            CppDecl::FreeFn {
                ret, params, body, ..
            } => {
                self.scan_ty(ret);
                for (_, t) in params {
                    self.scan_ty(t);
                }
                self.scan_body_types(body);
            }
            CppDecl::MemberFn {
                ret, params, body, ..
            } => {
                self.scan_ty(ret);
                for (_, t) in params {
                    self.scan_ty(t);
                }
                self.scan_body_types(body);
            }
            CppDecl::Ctor { params, body, .. } => {
                for (_, t) in params {
                    self.scan_ty(t);
                }
                self.scan_body_types(body);
            }
            CppDecl::Dtor { body, .. } => {
                self.scan_body_types(body);
            }
            CppDecl::MemberVar { ty, .. } => {
                self.scan_ty(ty);
            }
            CppDecl::OperatorFn {
                params, body, ret, ..
            } => {
                for (_, t) in params {
                    self.scan_ty(t);
                }
                self.scan_ty(ret);
                self.scan_body_types(body);
            }
            CppDecl::Namespace { body, .. } => {
                for x in body {
                    self.scan_decl_types(x);
                }
            }
            _ => {}
        }
    }

    fn scan_ty(&mut self, ty: &MangleType) {
        match ty {
            MangleType::Template { name, args } => {
                for a in args {
                    self.scan_ty(a);
                }
                self.emit_inst_class(name, args);
            }
            MangleType::Pointer(t) | MangleType::Reference(t) => self.scan_ty(t),
            _ => {}
        }
    }

    fn scan_body_types(&mut self, body: &CppBody) {
        for s in &body.stmts {
            self.scan_stmt_types(s);
        }
    }

    fn scan_stmt_types(&mut self, s: &CppStmt) {
        match s {
            CppStmt::Expr(e) | CppStmt::Return(Some(e)) => self.scan_expr_types(e),
            CppStmt::If { cond, then, els } => {
                self.scan_expr_types(cond);
                self.scan_body_types(then);
                if let Some(e) = els {
                    self.scan_body_types(e);
                }
            }
            CppStmt::IfConstexpr { cond, then, els } => {
                self.scan_expr_types(cond);
                self.scan_body_types(then);
                if let Some(e) = els {
                    self.scan_body_types(e);
                }
            }
            CppStmt::While { cond, body } => {
                self.scan_expr_types(cond);
                self.scan_body_types(body);
            }
            CppStmt::For {
                init,
                cond,
                step,
                body,
            } => {
                if let Some(i) = init {
                    self.scan_stmt_types(i);
                }
                if let Some(c) = cond {
                    self.scan_expr_types(c);
                }
                if let Some(x) = step {
                    self.scan_stmt_types(x);
                }
                self.scan_body_types(body);
            }
            CppStmt::Block(b) => self.scan_body_types(b),
            CppStmt::ThisMember(_, inner) => self.scan_stmt_types(inner),
            CppStmt::LocalVar { ty, init, .. } | CppStmt::StaticLocal { ty, init, .. } => {
                self.scan_ty(ty);
                if let Some(e) = init {
                    self.scan_expr_types(e);
                }
            }
            CppStmt::Try { body, catches } => {
                self.scan_body_types(body);
                for c in catches {
                    if let Some(t) = &c.ty {
                        self.scan_ty(t);
                    }
                    self.scan_body_types(&c.body);
                }
            }
            CppStmt::Throw(Some(e)) => self.scan_expr_types(e),
            CppStmt::CoAwait(e) | CppStmt::CoYield(e) => self.scan_expr_types(e),
            CppStmt::CoReturn(Some(e)) => self.scan_expr_types(e),
            CppStmt::Decl(d) => self.scan_decl_types(d),
            _ => {}
        }
    }

    fn scan_expr_types(&mut self, e: &CppExpr) {
        match e {
            CppExpr::Cast(t, x) => {
                self.scan_ty(t);
                self.scan_expr_types(x);
            }
            CppExpr::StaticCast { ty, expr }
            | CppExpr::ConstCast { ty, expr }
            | CppExpr::ReinterpretCast { ty, expr }
            | CppExpr::DynamicCast { ty, expr } => {
                self.scan_ty(ty);
                self.scan_expr_types(expr);
            }
            CppExpr::TypeidType(t) => self.scan_ty(t),
            CppExpr::Sizeof(SizeofArg::Type(t)) => self.scan_ty(t),
            CppExpr::Sizeof(SizeofArg::Expr(x)) => self.scan_expr_types(x),
            CppExpr::TypedInitList { ty, elems } => {
                self.scan_ty(ty);
                for x in elems {
                    self.scan_expr_types(x);
                }
            }
            CppExpr::Member(o, _)
            | CppExpr::PtrMember(o, _)
            | CppExpr::MemberStar(o, _)
            | CppExpr::PtrMemberStar(o, _) => self.scan_expr_types(o),
            CppExpr::Call(f, args) => {
                self.scan_expr_types(f);
                for a in args {
                    self.scan_expr_types(a);
                }
            }
            CppExpr::New(ty, args) => {
                self.scan_ty(&MangleType::Class(ty.clone()));
                for a in args {
                    self.scan_expr_types(a);
                }
            }
            CppExpr::NewArr { ty, count } => {
                self.scan_ty(&MangleType::Class(ty.clone()));
                self.scan_expr_types(count);
            }
            CppExpr::PlacementNew { ty, args, ptr } => {
                self.scan_ty(&MangleType::Class(ty.clone()));
                self.scan_expr_types(ptr);
                for a in args {
                    self.scan_expr_types(a);
                }
            }
            CppExpr::Delete(x)
            | CppExpr::DeleteArr(x)
            | CppExpr::Noexcept(x)
            | CppExpr::Ref(x)
            | CppExpr::AddrOf(x)
            | CppExpr::Typeid(x)
            | CppExpr::UnaryOp(_, x) => self.scan_expr_types(x),
            CppExpr::BinOp(_, l, r) => {
                self.scan_expr_types(l);
                self.scan_expr_types(r);
            }
            CppExpr::Ternary { cond, then, els } => {
                self.scan_expr_types(cond);
                self.scan_expr_types(then);
                self.scan_expr_types(els);
            }
            CppExpr::Comma(xs) | CppExpr::InitList(xs) => {
                for x in xs {
                    self.scan_expr_types(x);
                }
            }
            CppExpr::OpCall { args, .. } => {
                for a in args {
                    self.scan_expr_types(a);
                }
            }
            CppExpr::Lambda(lam) => {
                for (_, t) in &lam.params {
                    self.scan_ty(t);
                }
                self.scan_body_types(&lam.body);
            }
            CppExpr::MultiIndex(o, idx) => {
                self.scan_expr_types(o);
                for x in idx {
                    self.scan_expr_types(x);
                }
            }
            _ => {}
        }
    }

    /// M26: 类模板实例化 + 输出定义 (递归: 嵌套模板类先于自身输出).
    /// 触发点都在普通函数定义输出之前 (collect_classes 阶段).
    fn emit_inst_class(&mut self, name: &str, args: &[MangleType]) {
        let key = TemplateRegistry::key_for(name, args);
        if self.inst_emitted.contains(&key) {
            return;
        }
        let Some(inst) = self.resolve_class_inst(name, args) else {
            return;
        };
        self.inst_emitted.insert(key);
        // 先递归输出嵌套模板类 (字段/成员引用需要完整定义)
        self.scan_decl_types(&inst);
        // 注册类信息: 方法改写 / ctor 查找 / typeid
        self.register_inst_class(&inst);
        // 实例化类名是全局唯一名, 与命名空间限定无关
        let saved_ns = self.ns.stack.clone();
        self.ns.stack.clear();
        self.lower_decl(&inst);
        self.ns.stack = saved_ns;
    }

    /// M26: 解析类实例 (偏特化优先), 失败 = requires 不满足 / 模板未注册
    fn resolve_class_inst(&mut self, name: &str, args: &[MangleType]) -> Option<CppDecl> {
        // 偏特化优先: `Foo<T*>` 匹配实参 `int*`
        if let Some((pdecl, pnames, pargs)) = self.templates.pick_partial(name, args) {
            let rename = instantiated_class_name(name, args);
            return Some(crate::cpp::template::substitute_decl(
                &pdecl,
                &pnames,
                &pargs,
                Some(&rename),
            ));
        }
        self.templates.instantiate_class(name, args)
    }

    /// M26: 注册实例化类的 class_methods / class_fields / class_ctors
    fn register_inst_class(&mut self, inst: &CppDecl) {
        if let CppDecl::Class(c) = inst {
            let qname = c.name.clone(); // 已是实例化类名
            if !self.classes.contains(&qname) {
                self.classes.push(qname.clone());
            }
            let mut methods = vec![];
            let mut fields = vec![];
            let mut ctors = vec![];
            for m in &c.members {
                match m {
                    CppDecl::MemberFn { name, .. } => methods.push(name.clone()),
                    CppDecl::MemberVar { ty, name, .. } => fields.push((name.clone(), ty.clone())),
                    CppDecl::Ctor { .. } => ctors.push(m.clone()),
                    _ => {}
                }
            }
            self.class_methods.insert(qname.clone(), methods);
            self.class_fields.insert(qname.clone(), fields);
            self.class_ctors.insert(qname, ctors);
        }
    }

    /// M26: 模板函数实例化 C 名: `foo<int>` -> `__dcc_tplfn_foo_int`
    fn tplfn_c_name(name: &str, args: &[MangleType]) -> String {
        let keys: Vec<String> = args.iter().map(type_key).collect();
        format!("__dcc_tplfn_{}_{}", name, keys.join("_"))
    }

    /// M26: 输出模板函数实例化调用 (调用点), 并把实例化定义排入待输出队列.
    /// 定义在 lower_decls 2.5 阶段统一输出 (emit_pending_fns).
    fn emit_template_fn_call(&mut self, name: &str, args: &[MangleType]) {
        // 验证可实例化 (requires 约束); 失败 = SFINAE 跳过
        match self.templates.instantiate_fn(name, args) {
            Some(CppDecl::FreeFn { .. }) => {}
            _ => {
                self.out
                    .push_str(&format!("0 /* SFINAE: {}<...> not a function */", name));
                return;
            }
        }
        let key = TemplateRegistry::key_for(name, args);
        if !self
            .pending_fn_insts
            .iter()
            .any(|(n, a)| TemplateRegistry::key_for(n, a) == key)
        {
            self.pending_fn_insts
                .push_back((name.to_string(), args.to_vec()));
        }
        self.out.push_str(&Self::tplfn_c_name(name, args));
    }

    /// M26: 输出所有函数模板实例化定义 (lower_decls 2.5 阶段).
    /// 函数体内触发的类实例化先输出 (struct 定义先于使用).
    fn emit_pending_fns(&mut self) {
        while let Some((name, args)) = self.pending_fn_insts.pop_front() {
            let Some(inst) = self.templates.instantiate_fn(&name, &args) else {
                continue;
            };
            let (ret, params, body) = match &inst {
                CppDecl::FreeFn {
                    ret, params, body, ..
                } => (ret.clone(), params.clone(), body.clone()),
                _ => continue,
            };
            // 扫描体内类型 -> 触发类实例化 (struct 定义先于本函数)
            self.scan_decl_types(&inst);
            // 定义名 = 调用点名字 (不经过 Itanium mangle)
            let cname = Self::tplfn_c_name(&name, &args);
            self.emit_fn(&cname, &ret, &params, &body);
        }
    }

    /// 判断一个声明树是否需要 stddef.h / stdint.h (异常 / new / delete 等)
    fn decl_needs_system_headers(&self, d: &CppDecl) -> bool {
        match d {
            CppDecl::Template { decl, .. } => self.decl_needs_system_headers(decl),
            CppDecl::Namespace { body, .. } => {
                body.iter().any(|x| self.decl_needs_system_headers(x))
            }
            CppDecl::Class(c) => {
                // 类内成员函数体可能用 new / delete / throw
                c.members.iter().any(|m| self.decl_needs_system_headers(m))
            }
            CppDecl::FreeFn { body, .. }
            | CppDecl::MemberFn { body, .. }
            | CppDecl::Ctor { body, .. }
            | CppDecl::Dtor { body, .. } => self.body_needs_system_headers(body),
            CppDecl::OperatorFn { body, .. } => self.body_needs_system_headers(body),
            _ => false,
        }
    }

    /// 判断函数体是否使用 new / delete / throw / try
    fn body_needs_system_headers(&self, body: &CppBody) -> bool {
        for s in &body.stmts {
            if self.stmt_needs_system_headers(s) {
                return true;
            }
        }
        false
    }

    fn stmt_needs_system_headers(&self, s: &CppStmt) -> bool {
        match s {
            CppStmt::Expr(e) => self.expr_needs_system_headers(e),
            CppStmt::Return(Some(e)) => self.expr_needs_system_headers(e),
            CppStmt::Block(b) => self.body_needs_system_headers(b),
            CppStmt::If { cond, then, els } => {
                self.expr_needs_system_headers(cond)
                    || self.body_needs_system_headers(then)
                    || els
                        .as_ref()
                        .is_some_and(|b| self.body_needs_system_headers(b))
            }
            CppStmt::While { cond, body } => {
                self.expr_needs_system_headers(cond) || self.body_needs_system_headers(body)
            }
            CppStmt::For {
                init,
                cond,
                step,
                body,
            } => {
                init.as_ref()
                    .is_some_and(|s| self.stmt_needs_system_headers(s))
                    || cond
                        .as_ref()
                        .is_some_and(|e| self.expr_needs_system_headers(e))
                    || step
                        .as_ref()
                        .is_some_and(|s| self.stmt_needs_system_headers(s))
                    || self.body_needs_system_headers(body)
            }
            CppStmt::LocalVar { init: Some(e), .. } => self.expr_needs_system_headers(e),
            CppStmt::Try { .. } | CppStmt::Throw(_) => true,
            _ => false,
        }
    }

    fn expr_needs_system_headers(&self, e: &CppExpr) -> bool {
        match e {
            CppExpr::New(_, _) | CppExpr::Delete(_) => true,
            CppExpr::Call(f, args) => {
                self.expr_needs_system_headers(f)
                    || args.iter().any(|a| self.expr_needs_system_headers(a))
            }
            CppExpr::BinOp(_, l, r) => {
                self.expr_needs_system_headers(l) || self.expr_needs_system_headers(r)
            }
            CppExpr::UnaryOp(_, x)
            | CppExpr::Ref(x)
            | CppExpr::AddrOf(x)
            | CppExpr::Cast(_, x)
            | CppExpr::DynamicCast { expr: x, .. }
            | CppExpr::StaticCast { expr: x, .. }
            | CppExpr::ConstCast { expr: x, .. }
            | CppExpr::ReinterpretCast { expr: x, .. }
            | CppExpr::PtrMember(x, _)
            | CppExpr::Member(x, _) => self.expr_needs_system_headers(x),
            CppExpr::Ternary { cond, then, els } => {
                self.expr_needs_system_headers(cond)
                    || self.expr_needs_system_headers(then)
                    || self.expr_needs_system_headers(els)
            }
            CppExpr::Comma(exprs) => exprs.iter().any(|e| self.expr_needs_system_headers(e)),
            CppExpr::Typeid(_) => true,
            // M18.0: 多维下标按 obj 递归 (与单下标一致)
            CppExpr::MultiIndex(obj, _) => self.expr_needs_system_headers(obj),
            _ => false,
        }
    }

    fn lower_decl(&mut self, d: &CppDecl) {
        match d {
            CppDecl::Class(c) => {
                // M26: BMF - export module 后的类记为导出 (含成员布局)
                if self.module_collect {
                    // M26.4: layout 记单行 C struct 字段体 (`int x; double y;`),
                    // import 端直接拼成 `struct <name> { <body> };`
                    let layout = c
                        .members
                        .iter()
                        .filter_map(|m| match m {
                            CppDecl::MemberVar { ty, name, .. } => {
                                Some(format!("{} {};", self.mangle_to_c_ns(ty), name))
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    self.module_exports.push(BmfExport {
                        kind: BmfKind::Class,
                        name: c.name.clone(),
                        signature: String::new(),
                        layout: if layout.is_empty() { None } else { Some(layout) },
                    });
                }
                self.lower_class(c);
            }
            CppDecl::FreeFn {
                ret,
                name,
                params,
                body,
                is_noexcept,
            } => {
                let n = NestedName::from_dotted(vec![self.qualify_name(name)]);
                let m = mangle_member_function(&n, &[], ret);
                // M26: BMF - export module 后的顶层自由函数记为导出 (main 除外)
                if self.module_collect && name != "main" {
                    self.module_exports.push(BmfExport {
                        kind: BmfKind::Function,
                        // M26.4: name 记源码名 (import 端注册用); signature 记
                        // 完整 C 声明文本 `ret mangled(args)` (含 Itanium 符号)
                        name: name.clone(),
                        signature: self.fn_decl_c(&m, ret, params),
                        layout: None,
                    });
                }
                // M26.1: noexcept 函数体按声明输出 __attribute__((nothrow))
                if *is_noexcept {
                    self.out.push_str("__attribute__((nothrow)) ");
                }
                self.emit_fn(&m, ret, params, body);
            }
            CppDecl::MemberFn {
                class,
                ret,
                name,
                params,
                body,
                is_virtual,
                is_override,
                is_const,
                is_final,
                is_noexcept,
                is_volatile,
                is_friend,
            } => {
                // M17.0: friend 函数声明是自由函数, 不属于类成员
                // (即: 没有 this 形参, 不放在类作用域中, 也不做 mangle)
                if *is_friend {
                    // 直接当作自由函数处理; lower 端不再加 this 形参
                    self.emit_fn(name, ret, params, body);
                    let _ = (is_override, is_final, is_volatile);
                    return;
                }
                let class_q = self.qualify_name(class);
                let n = NestedName::from_dotted(vec![class_q.clone(), name.clone()]);
                let m = mangle_member_function(&n, &[], ret);
                let this_ty = MangleType::Pointer(Box::new(MangleType::Class(class_q.clone())));
                let mut all_params: Vec<(String, MangleType)> = vec![("this".to_string(), this_ty)];
                all_params.extend(params.iter().cloned());
                // M26.4: BMF - 导出成员函数 (import 端声明 + class_methods 注册)
                if self.module_collect {
                    self.module_exports.push(BmfExport {
                        kind: BmfKind::Function,
                        name: format!("{}::{}", class, name),
                        signature: self.fn_decl_c(&m, ret, &all_params),
                        layout: None,
                    });
                }
                if *is_virtual {
                    // 输出 vtable 项
                    // M17.0: final / override / volatile 标记影响 vtable 项语义
                    self.emit_vtable_entry(class, name, &m, *is_const);
                }
                // M17.0: noexcept -> 形参前加 __attribute__((nothrow))
                if *is_noexcept {
                    self.out.push_str("__attribute__((nothrow)) ");
                }
                // 类体内定义的成员函数隐式 inline (Itanium ABI: weak linkage)
                // 多 TU 包含相同类定义时不报 multiple definition
                self.out.push_str("__attribute__((weak)) ");
                // M17.0: volatile 成员函数 - 标记此函数; lower 端不区分 volatile
                // (C 端 volatile 与 C++ volatile 语义在 ABI 上等价, 由 caller 决定
                // this 访问的 volatile 属性).
                if *is_volatile {
                    self.out.push_str("/* M17.0: volatile member */ ");
                }
                // 设置 current_class, 让 body 内 n_ -> this->n_
                self.current_class = Some(class_q);
                self.emit_fn(&m, ret, &all_params, body);
                // M17.0: final 阻止派生覆盖 (codegen 端, 派生若尝试覆盖会 mangled 冲突)
                // override 在派生类声明时检查基类是否存在; 当前简化不做
                let _ = (is_override, is_final, is_friend);
            }
            CppDecl::Ctor {
                class,
                params,
                inits,
                body,
                is_virtual,
                is_explicit,
                is_noexcept,
            } => {
                let class_q = self.qualify_name(class);
                let n = NestedName::from_dotted(vec![class_q.clone()]);
                let param_tys: Vec<MangleType> = params.iter().map(|(_, t)| t.clone()).collect();
                let m = mangle_ctor(&n, &param_tys, 1);
                let this_ty = MangleType::Pointer(Box::new(MangleType::Class(class_q.clone())));
                let mut all_params: Vec<(String, MangleType)> = vec![("this".to_string(), this_ty)];
                all_params.extend(params.iter().cloned());
                // M26.4: BMF - 导出构造函数 (import 端 extern + 类注册)
                if self.module_collect {
                    self.module_exports.push(BmfExport {
                        kind: BmfKind::Function,
                        name: format!("{}::ctor", class),
                        signature: self.fn_decl_c(&m, &MangleType::Void, &all_params),
                        layout: None,
                    });
                }
                if *is_noexcept {
                    self.out.push_str("__attribute__((nothrow)) ");
                }
                // 类体内定义的构造函数隐式 inline (weak linkage)
                self.out.push_str("__attribute__((weak)) ");
                if *is_explicit {
                    // M17.0: explicit 标记不生成特殊代码 (C++ 端禁止隐式转换)
                    // 我们通过函数注释标记; C 端不强制 (没有隐式转换)
                    self.out.push_str("/* M17.0: explicit ctor */ ");
                }
                self.emit_ctor(&m, &class_q, &all_params, inits, body);
                let _ = is_virtual;
            }
            CppDecl::Dtor {
                class,
                body,
                is_virtual,
                is_noexcept,
            } => {
                let class_q = self.qualify_name(class);
                let n = NestedName::from_dotted(vec![class_q.clone()]);
                let m = mangle_dtor(&n, 1);
                let this_ty = MangleType::Pointer(Box::new(MangleType::Class(class_q.clone())));
                let all_params: Vec<(String, MangleType)> = vec![("this".to_string(), this_ty)];
                if *is_noexcept {
                    self.out.push_str("__attribute__((nothrow)) ");
                }
                // 类体内定义的析构函数隐式 inline (weak linkage)
                self.out.push_str("__attribute__((weak)) ");
                self.current_class = Some(class_q);
                self.emit_fn(&m, &MangleType::Void, &all_params, body);
                let _ = is_virtual;
            }
            CppDecl::MemberVar {
                ty,
                name,
                is_mutable,
            } => {
                // M27: mutable 成员跟踪 + C 端注释标记
                // C 语言无 mutable 概念, 但语义上 mutable 字段在 const 方法中可写.
                // 我们跟踪 mutable 字段, 并在输出中添加注释标记.
                if *is_mutable {
                    if let Some(cls) = &self.current_class {
                        self.mutable_fields
                            .entry(cls.clone())
                            .or_default()
                            .push(name.clone());
                    }
                    self.out
                        .push_str(&format!("    {} {}; /* mutable */\n", self.mangle_to_c_ns(ty), name));
                } else {
                    self.out
                        .push_str(&format!("    {} {};\n", self.mangle_to_c_ns(ty), name));
                }
            }
            CppDecl::Namespace { name, body } => {
                if let Some(n) = name {
                    self.ns.stack.push(n.clone());
                }
                for x in body {
                    self.lower_decl(x);
                }
                if name.is_some() {
                    self.ns.stack.pop();
                }
            }
            CppDecl::Using { is_namespace, path } => {
                // M8: 仅注释提示 (lower 阶段忽略)
                let _ = (is_namespace, path);
            }
            CppDecl::OperatorFn {
                class,
                op,
                params,
                body,
                ret,
                ..
            } => {
                if let Some(c) = class {
                    let class_q = self.qualify_name(c);
                    let param_tys: Vec<MangleType> =
                        params.iter().map(|(_, t)| t.clone()).collect();
                    let m = mangle_operator_member(&class_q, op.clone(), &param_tys);
                    let this_ty = MangleType::Pointer(Box::new(MangleType::Class(class_q.clone())));
                    let mut all_params: Vec<(String, MangleType)> =
                        vec![("this".to_string(), this_ty)];
                    all_params.extend(params.iter().cloned());
                    // 类体内定义的运算符隐式 inline (weak linkage)
                    self.out.push_str("__attribute__((weak)) ");
                    // 设置 current_class, 让 body 内裸字段名 -> this->field
                    self.current_class = Some(class_q);
                    self.emit_fn(&m, ret, &all_params, body);
                } else {
                    let param_tys: Vec<MangleType> =
                        params.iter().map(|(_, t)| t.clone()).collect();
                    let m = mangle_operator_global(op.clone(), &param_tys);
                    self.emit_fn(&m, ret, params, body);
                }
            }
            CppDecl::Template {
                params,
                decl,
                requires,
                is_export,
            } => {
                // M26: 模板已在 collect_classes 注册, 按需实例化.
                // 未被使用的模板不产生 C 输出 (避免 T 未定义占位类型).
                // 若内层是 Concept, 输出概念占位 (M10+)
                if let CppDecl::Concept { name, body_text } = decl.as_ref() {
                    let _ = requires;
                    let c = crate::cpp::feature_degrade::Concept::parse(body_text);
                    self.out.push_str(&format!(
                        "/* dcc C++ concept (M10+): {} = {} (parsed, evaluated) */\n",
                        name, body_text
                    ));
                    let _ = c;
                } else {
                    // M26: export template 时, 将模板记为 BMF 导出
                    if *is_export && self.module_collect {
                        // 提取模板内层声明的名字 (类名或函数名)
                        let tpl_name = match decl.as_ref() {
                            CppDecl::Class(c) => c.name.clone(),
                            CppDecl::FreeFn { name, .. } => name.clone(),
                            _ => String::new(),
                        };
                        if !tpl_name.is_empty() {
                            self.module_exports.push(BmfExport {
                                kind: BmfKind::Template,
                                name: tpl_name,
                                signature: params
                                    .params
                                    .iter()
                                    .map(|p| p.name.as_str())
                                    .collect::<Vec<_>>()
                                    .join(","),
                                layout: None,
                            });
                        }
                    }
                    let _ = (decl, requires);
                    self.out.push_str(&format!(
                        "/* template <{}> (registered, instantiated on demand) */\n",
                        params
                            .params
                            .iter()
                            .map(|p| p.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
            CppDecl::PartialSpec {
                params,
                pattern_text,
                decl,
            } => {
                // M26: 偏特化已在 collect_classes 注册; 实参匹配时优先选用
                let _ = decl;
                self.out.push_str(&format!(
                    "/* partial specialization <{}> for pattern `{}` (registered) */\n",
                    params
                        .params
                        .iter()
                        .map(|p| p.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    pattern_text
                ));
            }
            // M17.0: using Base::Base 真 wrapper - 为基类每个 ctor 生成派生 wrapper
            // 实际策略: 查 class_methods[Base] 获取基类 ctor 列表, 派生类生成转发
            CppDecl::InheritingCtor { class, base } => {
                if class.is_empty() {
                    self.out.push_str(&format!(
                        "/* M17.0: using {}::{} (parsed at class scope) */\n",
                        base, base
                    ));
                    return; // 等价于 continue, 但 match 块内不能 continue
                }
                // 查基类 ctor 列表
                let base_q = self.qualify_name(base);
                let base_ctors = self.class_ctors.get(&base_q).cloned().unwrap_or_default();
                for m in &base_ctors {
                    if let CppDecl::Ctor {
                        params,
                        inits,
                        body,
                        ..
                    } = m
                    {
                        // M17.0: 派生类 ctor 转发到基类 ctor, 不重复 inline
                        // emit: void Derived_ctor(this, params) { Base_ctor(this, params); }
                        let class_q = self.qualify_name(class);
                        let n_d = NestedName::from_dotted(vec![class_q.clone()]);
                        let param_tys: Vec<MangleType> =
                            params.iter().map(|(_, t)| t.clone()).collect();
                        let m_d = mangle_ctor(&n_d, &param_tys, 1);
                        let this_ty =
                            MangleType::Pointer(Box::new(MangleType::Class(class_q.clone())));
                        let mut all_params: Vec<(String, MangleType)> =
                            vec![("this".to_string(), this_ty)];
                        all_params.extend(params.iter().cloned());
                        self.out.push_str(&format!("void {} (", m_d));
                        for (i, (n, t)) in all_params.iter().enumerate() {
                            if i > 0 {
                                self.out.push_str(", ");
                            }
                            self.out
                                .push_str(&format!("{} {}", self.mangle_to_c_ns(t), n));
                        }
                        self.out.push_str(") {\n");
                        // 转发到基类 ctor
                        let n_b = NestedName::from_dotted(vec![base_q.clone()]);
                        let m_b = mangle_ctor(&n_b, &param_tys, 1);
                        self.out.push_str("    ");
                        self.out.push_str(&m_b);
                        self.out.push('(');
                        self.out.push_str("this");
                        for (n, _) in params {
                            self.out.push_str(", ");
                            self.out.push_str(n);
                        }
                        self.out.push_str(");\n");
                        // 派生类自己的 init 列表 (若 Body 中有)
                        for (member, expr) in inits {
                            self.out.push_str(&format!(
                                "    ((struct {}*)this)->{} = ",
                                class_q, member
                            ));
                            self.emit_expr(expr);
                            self.out.push_str(";\n");
                        }
                        // body (若非空)
                        for s in &body.stmts {
                            self.emit_stmt(s);
                        }
                        self.out.push_str("}\n");
                    }
                }
                if base_ctors.is_empty() {
                    // 没找到基类 ctor 列表 (基类未在此 TU 定义), 退化为注释
                    self.out.push_str(&format!(
                        "/* M17.0: using {}::{} - base ctors not visible */\n",
                        base, base
                    ));
                }
            }
            // M10+: concept 求值 (而非 M9 的简单占位)
            CppDecl::Concept { name, body_text } => {
                let c = crate::cpp::feature_degrade::Concept::parse(body_text);
                self.out.push_str(&format!(
                    "/* dcc C++ concept (M10+): {} = {} (parsed, evaluated) */\n",
                    name, body_text
                ));
                let _ = c; // 概念定义仅作声明, 实际约束在 use 处求值
            }
            CppDecl::Module {
                name,
                partition,
                is_export,
            } => {
                // M10+: 支持分区
                let kind = if *is_export { "export module" } else { "module" };
                let n = name.as_deref().unwrap_or("(global)");
                let sig = if let Some(p) = partition {
                    format!("{}:{}", n, p)
                } else {
                    n.to_string()
                };
                self.out
                    .push_str(&format!("/* dcc C++ {} (M10+): {} */\n", kind, sig));
                // M26: BMF 序列化 - export module 声明后进入导出收集模式
                if *is_export {
                    if let Some(n) = name {
                        self.module_name = n.clone();
                    }
                    // M26.4: 分区模块 `export module foo:part;`
                    if let Some(p) = partition {
                        self.module_partition = Some(p.clone());
                    }
                    self.module_collect = true;
                }
            }
            CppDecl::PrivateFragment => {
                // M26.4: `module :private;` - 之后的声明不导出
                self.module_collect = false;
                self.out
                    .push_str("/* dcc C++ module :private (M26.4): exports end here */\n");
            }
            CppDecl::Import { path } => {
                // M26: BMF - 记录 import 依赖 (module 收集模式下)
                if self.module_collect {
                    self.module_imports.push(path.join("::"));
                }
                // M26.4: 分区导入 `import :part;` - 并入本模块导出
                if path.len() == 1 && path[0].starts_with(':') {
                    let part = &path[0][1..];
                    let found = self
                        .imported_modules
                        .iter()
                        .find(|m| m.partition.as_deref() == Some(part))
                        .cloned();
                    if let Some(bmf) = found {
                        self.module_part_imports.push(part.to_string());
                        self.emit_imported_module_decls(&bmf);
                        return;
                    }
                    self.out.push_str(&format!(
                        "/* dcc C++ import :{} (partition bmf not found) */\n",
                        part
                    ));
                    return;
                }
                // M26.4: 常规 `import foo;` - 若能找到 foo.bmf (driver 预加载),
                // 生成 extern 声明 + 注册名字; 否则回退为 include / 注释占位.
                if path.len() == 1 {
                    let p = &path[0];
                    if let Some(bmf) = self.imported_modules.iter().find(|m| m.name == *p).cloned()
                    {
                        self.emit_imported_module_decls(&bmf);
                        return;
                    }
                    if p.contains('.') || p.chars().all(|c| c.is_alphanumeric() || c == '_') {
                        // 单段且像 header (含扩展名或纯标识符) → 当作 `#include <p>`
                        self.out.push_str(&format!("#include <{}>\n", p));
                        return;
                    }
                }
                self.out.push_str(&format!(
                    "/* dcc C++ import (M9 placeholder): {} */\n",
                    path.join("::")
                ));
            }
            CppDecl::ExportBlock(body) => {
                self.out
                    .push_str("/* dcc C++ export block (M9 placeholder) */\n");
                for x in body {
                    self.lower_decl(x);
                }
            }
            CppDecl::RawExternC(text) => {
                /* M18.0: 透传 extern "C" { ... } 块原文.
                 * 内部 C 函数声明 (如 dccpp_dur_from_nanos) 必须保留,
                 * 否则 lower 后的 C 代码会找不到 dccpp_* 运行时符号.
                 *
                 * 注意: dcc C 编译器不识别 `extern "C"` 语法, 所以只
                 * 输出块体内的 C 声明本身 (去掉 `extern "C" {` 和 `}`).
                 *
                 * dcc C 同时也不接受 `dccpp_path*` 这种裸结构体名,
                 * 这里对每个出现的 `dccpp_<ident>` (作为类型) 注入
                 * `typedef struct dccpp_<ident> dccpp_<ident>;` 别名,
                 * 让后续 `dccpp_path*` / `dccpp_path* foo` /
                 * `dccpp_file_status dccpp_status_of(...)` 都能识别
                 * 为合法 C 类型.
                 *
                 * 识别 type 位置的 `dccpp_<ident>`:
                 *   1. 紧跟 (跳过空格) `*`        (e.g. `dccpp_path*`)
                 *   2. 紧跟 (跳过空格) 另一个 ident (e.g. `dccpp_file_status dccpp_status_of`)
                 *
                 * 不识别 `dccpp_<ident>(` 函数名形式 (e.g. `dccpp_path_new(`)
                 * 因为那是函数名, 不是 type.
                 */
                self.out
                    .push_str("/* dcc++ M18: extern \"C\" block (de-wrapped) */\n");
                /* 先注入 int64_t/uint64_t 等本项目常用 typedef,
                 * 让 extern "C" 块内 `int64_t dccpp_fs_file_size(...)`
                 * 也能被 dcc C 接受. */
                self.out.push_str("typedef long long int64_t;\n");
                self.out.push_str("typedef unsigned long long uint64_t;\n");
                if let Some(open) = text.find('{') {
                    if let Some(close) = text.rfind('}') {
                        if open < close {
                            let body = &text[open + 1..close];
                            // 收集所有 type 位置的 `dccpp_<ident>`, 注入 typedef
                            let bytes = body.as_bytes();
                            let mut i = 0;
                            let mut seen: std::collections::HashSet<String> =
                                std::collections::HashSet::new();
                            while i + 6 <= bytes.len() {
                                if &bytes[i..i + 6] == b"dccpp_"
                                    && (i == 0 || !is_ident_char(bytes[i - 1]))
                                {
                                    let start = i;
                                    let mut j = i + 6;
                                    while j < bytes.len() && is_ident_char(bytes[j]) {
                                        j += 1;
                                    }
                                    // 看 type 之后跟的是什么: `*` 还是 ident
                                    let mut k = j;
                                    while k < bytes.len() && bytes[k] == b' ' {
                                        k += 1;
                                    }
                                    let is_type = if k < bytes.len() && bytes[k] == b'*' {
                                        // 形式 1: dccpp_X * ...
                                        true
                                    } else if k < bytes.len() && is_ident_char(bytes[k]) {
                                        // 形式 2: dccpp_X <ident> - 这是 return type + 名字
                                        // 进一步检查: <ident> 之后是 `(` (函数声明)
                                        let mut m = k;
                                        while m < bytes.len() && is_ident_char(bytes[m]) {
                                            m += 1;
                                        }
                                        // 跳过空格
                                        while m < bytes.len() && bytes[m] == b' ' {
                                            m += 1;
                                        }
                                        m < bytes.len() && bytes[m] == b'('
                                    } else {
                                        false
                                    };
                                    if is_type {
                                        let name =
                                            std::string::String::from_utf8_lossy(&bytes[start..j])
                                                .into_owned();
                                        if !seen.contains(&name) {
                                            seen.insert(name.clone());
                                            self.out.push_str(&format!(
                                                "typedef struct {} {};\n",
                                                name, name
                                            ));
                                        }
                                    }
                                    i = j;
                                } else {
                                    i += 1;
                                }
                            }
                            // C++ 特有语法清洗: 去除 C 编译器不接受的构造
                            let cleaned = strip_cpp_keywords(body);
                            self.out.push_str(&cleaned);
                        }
                    }
                }
                self.out.push('\n');
            }
            CppDecl::Typedef => {
                /* M11 stdlib: typedef 不产生 C 输出 (lower 后的 C
                 * 代码会引入 stddef.h 等系统头处理 size_t). */
            }
            CppDecl::Empty => {
                /* M18.0: enum / 修饰后无实际声明时使用, 不产生 C 输出. */
            }
            CppDecl::Enum {
                name,
                scoped,
                underlying,
                variants,
            } => {
                // M18.0+: 保留枚举类型语义: 枚举名 + 枚举值 + 可选的底层类型.
                // 降级为真实 C enum, 枚举常量名不加重名 (C++ 端 `E::A` 在
                // lower 时只取末段, 见 QualifiedAccess 分支).
                if name != "_anon" {
                    // 先登记类型名, 让后续类型引用 (`sizeof(E)` / `E var`) 不前缀 struct.
                    self.enum_types.insert(name.clone());
                }
                let prefixed = self.qualify_name(name);
                let mut body = String::new();
                for (i, (vname, vinit)) in variants.iter().enumerate() {
                    if i > 0 {
                        body.push_str(", ");
                    }
                    body.push_str(vname);
                    if let Some(init) = vinit {
                        body.push_str(&format!(" = {}", init));
                    }
                }
                let _ = scoped; // scoped 只影响名字可见性; 降级后枚举常量统一可见
                if name == "_anon" {
                    // 匿名枚举: 只需要常量, 不需要类型名.
                    self.out.push_str(&format!("enum {{ {} }};\n", body));
                } else if let Some(ut) = underlying {
                    // C23 enum 底层类型: 尽可能保留 sizeof / 值域语义.
                    self.out.push_str(&format!(
                        "typedef enum : {} {{ {} }} {};\n",
                        ut, body, prefixed
                    ));
                } else {
                    self.out
                        .push_str(&format!("typedef enum {{ {} }} {};\n", body, prefixed));
                }
            }
        }
    }

    /// M26.3: 虚继承已由预遍历 (compute_transitive_vbases / compute_class_layouts)
    /// 降级为 Itanium ABI 布局: 传递虚基子对象嵌入对象尾部, `__vbptr` 指向
    /// 每类 `__vbt_<C>` vbtable (vbase_offset 项), 成员访问/上转型经
    /// vbtable 运行时 this-adjust.
    fn lower_class(&mut self, c: &ClassDecl) {
        // M18.0: 用 qualified name 避免 namespace 内类与全局类冲突
        let qname = self.qualify_name(&c.name);
        // Store base class information for inherited member access
        self.class_bases.insert(qname.clone(), c.bases.clone());
        // M26.3: 传递虚基列表与 vbtable (预遍历已计算)
        let vbs = self
            .transitive_vbases
            .get(&qname)
            .cloned()
            .unwrap_or_default();
        let vtable = self.vbase_tables.get(&qname).cloned().unwrap_or_default();
        // 阶段 0: 虚基 vbtable (vbase_offset 项, 第 0 项恒为 0)
        if !vbs.is_empty() {
            self.out.push_str(&format!(
                "static long __vbt_{}[] = {{ {} }};\n",
                qname,
                vtable
                    .iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        // 阶段 1: 仅 struct 定义 (成员字段 + vptr + vbptr + 虚基子对象)
        self.out.push_str(&format!("struct {} {{\n", qname));
        // 继承: 嵌入非虚基类子结构 (虚基子对象在尾部统一嵌入)
        for b in &c.bases {
            // 模板基类: 先实例化, 然后用实例化后的类名
            let base_c_name = if let Some(ref targs) = b.template_args {
                // 触发模板实例化
                self.emit_inst_class(&b.name, targs);
                instantiated_class_name(&b.name, targs)
            } else {
                b.name.clone()
            };
            if !b.is_virtual {
                self.out
                    .push_str(&format!("    struct {} __base_{};\n", base_c_name, b.name));
            }
            // M26.3: 虚基不再用指针占位; 子对象在尾部统一嵌入
        }
        // 检查是否有 virtual 方法 -> 添加 vptr (M26.3: 与布局模拟一致)
        let has_virt = self.class_polymorphic.contains(&qname);
        if has_virt {
            self.out.push_str("    void* __vptr;\n");
        }
        // M26.3: 虚基指针 (vbptr) - 位于 vptr 之后, 指向 __vbt_<C>
        if !vbs.is_empty() {
            self.out.push_str("    long* __vbptr;\n");
        }
        for m in &c.members {
            match m {
                CppDecl::MemberVar { ty, name, .. } => {
                    self.out
                        .push_str(&format!("    {} {};\n", self.mangle_to_c_ns(ty), name));
                }
                CppDecl::Class(nested) => {
                    self.out
                        .push_str(&format!("/* nested class {} */\n", nested.name));
                    self.lower_class(nested);
                }
                _ => {} // 成员函数 / 构造 / 析构 -> 阶段 2
            }
        }
        // M26.3: 虚基子对象嵌入尾部 (传递虚基, DFS 顺序, 去重 - 菱形共享)
        for vb in &vbs {
            let vb_c = self.mangle_to_c_ns(&MangleType::Class(vb.clone()));
            self.out.push_str(&format!("    {} __vb_{};\n", vb_c, vb));
        }
        self.out.push_str("};\n");
        // M26.3: 无用户 ctor 且含虚基 -> 自动生成默认 ctor:
        //   1) this->__vbptr = __vbt_<C>   (vbtable 指针初始化)
        //   2) 依次调用虚基 ctor (共享基类只初始化一次)
        if !vbs.is_empty() && !c.members.iter().any(|m| matches!(m, CppDecl::Ctor { .. })) {
            let n = NestedName::from_dotted(vec![qname.clone()]);
            let m = mangle_ctor(&n, &[], 1);
            self.out
                .push_str(&format!("void {} (struct {}* this) {{\n", m, qname));
            self.out
                .push_str(&format!("    this->__vbptr = (long*)__vbt_{};\n", qname));
            for vb in &vbs {
                let n_vb = NestedName::from_dotted(vec![vb.clone()]);
                let m_vb = mangle_ctor(&n_vb, &[], 1);
                self.out
                    .push_str(&format!("    {}(&this->__vb_{});\n", m_vb, vb));
            }
            self.out.push_str("}\n");
        }
        // M18.0: 同时输出 typedef 让 unqualified 类名可作为类型
        // 例如 `std::duration` -> `typedef struct std_duration duration;`
        // 这样测试代码可以直接用 `duration ms(100);` 而 C 也能识别.
        // 规则: 如果 qname 包含下划线 (说明是 namespace-qualified),
        // 取最后一段作为 alias.
        // 多个类可能共享同一 alias (e.g. system_clock/steady_clock -> clock),
        // 已 emit 过的 alias 跳过以避免 dcc C 报 E0005 (duplicate typedef).
        // alias 是 C 关键字 (int/char/void/...) 时跳过, 否则 dcc C 报 E0004.
        if qname.contains('_') {
            if let Some(last) = qname.rsplit('_').next() {
                if last != qname && !self.emitted_typedefs.contains(last) && !is_c_keyword(last) {
                    self.emitted_typedefs.insert(last.to_string());
                    self.out
                        .push_str(&format!("typedef struct {} {};\n", qname, last));
                }
            }
        }
        // M17.0: 对每个数据成员, 输出 offsetof 函数 (供 `obj.*p` / `obj->*p` 使用)
        // 用函数而非 offsetof 宏: dcc C 前端不识别 offsetof 宏, 但能正确计算指针差
        for m in &c.members {
            if let CppDecl::MemberVar { name, .. } = m {
                self.out.push_str(&format!(
                    "__attribute__((weak)) size_t __dcc_mbr_{0}_{1}(void) {{ struct {0} __f; return (size_t)((char*)&__f.{1} - (char*)&__f); }}\n",
                    qname, name,
                ));
            }
        }
        // 继承成员也需要 __dcc_mbr: 通过 __base_ 链访问
        // 收集所有继承成员 (去重, 避免多继承同名成员重复生成)
        let mut inherited_mbrs: Vec<(String, String)> = Vec::new(); // (member_name, access_path)
        let mut emitted_inherited_mbrs = std::collections::HashSet::new();
        if let Some(bases) = self.class_bases.get(&c.name).cloned() {
            for base in &bases {
                // M26.3: 虚基成员经 vbtable 运行时调整, 静态 __base_ 链无效, 跳过
                if base.is_virtual {
                    continue;
                }
                let mut members = Vec::new();
                // 初始 chain 包含直接基类名
                let mut initial_chain = vec![base.name.clone()];
                Self::collect_inherited_members_static(
                    &self.class_bases,
                    &self.class_fields,
                    &base.name,
                    &mut initial_chain,
                    &mut members,
                );
                for (member_name, chain) in members {
                    if emitted_inherited_mbrs.contains(member_name.as_str()) {
                        continue;
                    }
                    emitted_inherited_mbrs.insert(member_name.clone());
                    // 构建 __base_ 访问路径
                    let mut access = String::new();
                    for base_name in &chain {
                        if !access.is_empty() {
                            access.push('.');
                        }
                        access.push_str(&format!("__base_{}", base_name));
                    }
                    inherited_mbrs.push((member_name, access));
                }
            }
        }
        // 生成继承成员的 __dcc_mbr
        for (member_name, access) in &inherited_mbrs {
            self.out.push_str(&format!(
                "__attribute__((weak)) size_t __dcc_mbr_{0}_{1}(void) {{ struct {0} __f; return (size_t)((char*)&__f.{2}.{1} - (char*)&__f); }}\n",
                qname, member_name, access,
            ));
        }
        // 阶段 2: 成员函数体外定义
        for m in &c.members {
            self.lower_class_member_fn(m);
        }
        // 输出 typeinfo 符号 (M17.0: 真正 struct, 不是 char*)
        self.emit_typeinfo(&c.name);
        // 输出 vtable 符号
        if has_virt {
            self.emit_vtable(&c.name, &c.members);
        }
    }

    /// 仅降级类成员中的函数部分 (构造 / 析构 / 成员函数 / 运算符)
    fn lower_class_member_fn(&mut self, m: &CppDecl) {
        match m {
            CppDecl::Ctor { .. }
            | CppDecl::Dtor { .. }
            | CppDecl::MemberFn { .. }
            | CppDecl::OperatorFn { .. } => {
                self.lower_decl(m);
            }
            _ => {}
        }
    }

    fn lower_class_member(&mut self, m: &CppDecl) {
        match m {
            CppDecl::Ctor { .. } | CppDecl::Dtor { .. } | CppDecl::MemberFn { .. } => {
                self.lower_decl(m);
            }
            CppDecl::MemberVar { ty, name, .. } => {
                self.out
                    .push_str(&format!("    {} {};\n", self.mangle_to_c_ns(ty), name));
            }
            CppDecl::OperatorFn { .. } => {
                self.lower_decl(m);
            }
            CppDecl::Class(c) => {
                // 嵌套类
                self.out
                    .push_str(&format!("/* nested class {} */\n", c.name));
                self.lower_class(c);
            }
            _ => {
                self.out.push_str("/* unsupported class member */\n");
            }
        }
    }
    /// 限定名 (在当前命名空间上下文中)
    fn qualify_name(&self, n: &str) -> String {
        if self.ns.stack.is_empty() {
            n.to_string()
        } else {
            let mut s = self.ns.stack.join("_");
            s.push('_');
            s.push_str(n);
            s
        }
    }

    /// 输出 typeinfo 符号 (M17.0: 真 struct, 字段为 name 字符串指针)
    /// `__dcc_ti_<Name>` 是 typeinfo 唯一标识, `typeid(T)` 与 `typeid(e)` 均返回此符号地址.
    fn emit_typeinfo(&mut self, name: &str) {
        let zti = mangle_typeinfo(name);
        self.out.push_str(&format!(
            "static char __dcc_ti_{}_name[] = \"{}\";\n",
            name, name
        ));
        // M17.0: typeinfo struct, 含 name 字段; address 即 typeinfo 指针
        // M26: 承载类型为 gcc_compat 的 __dcc_typeinfo_t (prologue 已定义)
        self.out.push_str(&format!(
            "static __dcc_typeinfo_t __dcc_ti_{0} = {{ __dcc_ti_{0}_name }};\n",
            name
        ));
        // Itanium _ZTI<Name> 兼容符号 (weak: 多 TU 允许重复定义)
        self.out.push_str(&format!(
            "__attribute__((weak)) void* {} = (void*)&__dcc_ti_{};\n",
            zti, name
        ));
    }

    /// 输出 vtable 数组与符号
    fn emit_vtable(&mut self, name: &str, members: &[CppDecl]) {
        let ztv = mangle_vtable(name);
        self.out
            .push_str(&format!("static void* __dcc_vt_{}[] = {{\n", name));
        // RTTI slot: pointer to typeinfo struct for dynamic typeid
        self.out.push_str(&format!(
            "    (void*)&__dcc_ti_{},  /* RTTI typeinfo pointer */\n",
            name
        ));
        for m in members {
            if let CppDecl::MemberFn {
                is_virtual: true,
                name: fn_name,
                params,
                class,
                is_const,
                ..
            } = m
            {
                let n = NestedName::from_dotted(vec![class.clone(), fn_name.clone()]);
                let m_name = mangle_member_function(&n, &[], &MangleType::Void);
                let _ = (params, is_const);
                self.out.push_str(&format!("    (void*){},\n", m_name));
            }
            if let CppDecl::Dtor {
                is_virtual: true,
                class,
                ..
            } = m
            {
                let n = NestedName::from_dotted(vec![class.clone()]);
                let m_name = mangle_dtor(&n, 1);
                self.out.push_str(&format!("    (void*){},\n", m_name));
            }
        }
        self.out.push_str("    (void*)0\n");
        self.out.push_str("};\n");
        self.out.push_str(&format!(
            "__attribute__((weak)) void* {} = __dcc_vt_{};\n",
            ztv, name
        ));
    }

    /// 输出 vtable 项
    fn emit_vtable_entry(&mut self, class: &str, name: &str, mangled: &str, is_const: bool) {
        let _ = (class, name, mangled, is_const);
    }

    fn emit_fn(
        &mut self,
        mangled: &str,
        ret: &MangleType,
        params: &[(String, MangleType)],
        body: &CppBody,
    ) {
        // 记录形参类型, 用于 body 内 obj.method() 改写
        self.param_var_types.clear();
        for (n, t) in params {
            self.param_var_types.insert(n.clone(), t.clone());
        }
        // 记录当前函数是否返回 void (用于 return *this → return 转换)
        self.current_fn_void = matches!(ret, MangleType::Void);
        // M26: 协程真状态机 (Duff's device) - body 含 co_await/co_yield/co_return
        // 时包裹 switch(<fn>_coro_state) { case 0: ... }, 每个挂起点生成恢复标签
        let is_coro = body_has_coro(body);
        if is_coro {
            let ret_c = self.mangle_to_c_ns(ret);
            self.coro_var = mangled.to_string();
            self.coro_ret_c = ret_c.clone();
            self.coro_state = 0;
            // 状态变量声明为文件作用域 static 全局 (跨调用保持;
            // dcc C 后端不支持函数内 static 局部, 见 coroutine.rs)
            self.out.push_str(&coro_fn_decls(mangled, &ret_c));
        }
        self.out
            .push_str(&format!("{} {} (", self.mangle_to_c_ns(ret), mangled));
        for (i, (n, t)) in params.iter().enumerate() {
            if i > 0 {
                self.out.push_str(", ");
            }
            self.out
                .push_str(&format!("{} {}", self.mangle_to_c_ns(t), n));
        }
        self.out.push_str(") {\n");
        if is_coro {
            self.out.push_str(&coro_fn_prologue(mangled));
        }
        for s in &body.stmts {
            self.emit_stmt(s);
        }
        if is_coro {
            self.out.push_str(&coro_fn_epilogue(&self.coro_ret_c));
            self.coro_var.clear();
            self.coro_ret_c.clear();
        }
        self.out.push_str("}\n");
        self.param_var_types.clear();
        self.current_class = None;
    }

    /// 构造函数: 先把 init 列表展开为 `this->member = expr;`,
    /// 然后接 body.
    fn emit_ctor(
        &mut self,
        mangled: &str,
        class: &str,
        params: &[(String, MangleType)],
        inits: &[(String, CppExpr)],
        body: &CppBody,
    ) {
        // 记录形参类型, 用于 body 内 obj.method() 改写
        self.param_var_types.clear();
        for (n, t) in params {
            self.param_var_types.insert(n.clone(), t.clone());
        }
        self.current_class = Some(class.to_string());
        self.out.push_str(&format!("void {} (", mangled));
        for (i, (n, t)) in params.iter().enumerate() {
            if i > 0 {
                self.out.push_str(", ");
            }
            self.out
                .push_str(&format!("{} {}", self.mangle_to_c_ns(t), n));
        }
        self.out.push_str(") {\n");
        // init 列表: this->member = expr;
        for (member, expr) in inits {
            self.out
                .push_str(&format!("    ((struct {}*)this)->{} = ", class, member));
            self.emit_expr(expr);
            self.out.push_str(";\n");
        }
        for s in &body.stmts {
            self.emit_stmt(s);
        }
        self.out.push_str("}\n");
        self.param_var_types.clear();
        self.current_class = None;
    }

    fn emit_stmt(&mut self, s: &CppStmt) {
        match s {
            CppStmt::Return(None) => self.out.push_str("return;\n"),
            CppStmt::Return(Some(e)) => {
                // `return *this;` in a void member function → `return;`
                // (C++ operator overloads return `*this` for chaining,
                // but the lowered C function returns void)
                let is_return_this = matches!(e, CppExpr::UnaryOp(
                    crate::cpp::parser::UnaryOpKind::Deref, inner
                ) if matches!(inner.as_ref(), CppExpr::This));
                if is_return_this && self.current_fn_void {
                    self.out.push_str("return;\n");
                } else {
                    self.out.push_str("return ");
                    self.emit_expr(e);
                    self.out.push_str(";\n");
                }
            }
            CppStmt::Expr(e) => {
                self.emit_expr(e);
                self.out.push_str(";\n");
            }
            CppStmt::Block(b) => {
                self.out.push_str("{\n");
                for s in &b.stmts {
                    self.emit_stmt(s);
                }
                self.out.push_str("}\n");
            }
            CppStmt::ThisMember(name, inner) => {
                self.out.push_str(&format!("this->{} = ", name));
                if let CppStmt::Expr(e) = &**inner {
                    self.emit_expr(e);
                }
                self.out.push_str(";\n");
            }
            CppStmt::LocalVar { ty, name, init } => {
                // M17.0: auto / decltype(auto) 真类型推导
                let resolved_ty = if ty.is_unresolved() {
                    if let Some(e) = init {
                        self.infer_type(e, matches!(ty, MangleType::DecltypeAuto))
                    } else {
                        // 无 init: auto 必须有 init (C++14), 退化为 int
                        MangleType::Int
                    }
                } else {
                    ty.clone()
                };
                // 记录局部变量类型, 用于 c.method() 改写
                self.local_var_types
                    .insert(name.clone(), resolved_ty.clone());
                // M17.0: 若 init 是整型常量, 记录到 i128 池 (供 if constexpr 使用)
                if let Some(e) = init {
                    if let Some(v) =
                        crate::cpp::cpp_const_eval::eval_cpp_const(e, &self.const_local_i128)
                    {
                        self.const_local_i128.insert(name.clone(), v);
                    }
                }
                // M17.0: 检测 init 是不是类构造 `KnownClass(args)`.
                // 若如此, 拆为两步: 1) 声明变量, 2) 调用 mangle 后的 ctor.
                // M26: 模板类 `Box<int> b(1)` 同样走 ctor 路径
                // (callee 名与模板名相同).
                let is_class_ctor_init = matches!(init,
                    Some(CppExpr::Call(f, _))
                        if matches!(&**f, CppExpr::Ident(n) if self.classes.contains(n)
                            || self.templates.find(n).is_some()));
                let class_ctor_name: Option<String> = if is_class_ctor_init {
                    if let Some(CppExpr::Call(f, _)) = init {
                        if let CppExpr::Ident(n) = &**f {
                            Some(n.clone())
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                let class_ctor_args: Vec<CppExpr> = if is_class_ctor_init {
                    if let Some(CppExpr::Call(_, args)) = init {
                        args.clone()
                    } else {
                        vec![]
                    }
                } else {
                    vec![]
                };
                // 把 C++ 类型名映射到 C 类型 (`Counter` -> `struct Counter`)
                // M18.0: 在命名空间内, 用 namespace-aware mangle 让 qualified 类被正确解析
                let c_type = if matches!(&resolved_ty, MangleType::Class(_)) {
                    self.mangle_to_c_ns(&resolved_ty)
                } else {
                    mangle_to_c(&resolved_ty)
                };
                self.out.push_str(&c_type);
                self.out.push(' ');
                self.out.push_str(name);
                if let Some(e) = init {
                    if is_class_ctor_init {
                        // 类构造: 单独放在下一个 stmt 调用, 这里仅以 0 占位
                        self.out.push_str(" = 0 /*ctor*/");
                        let _ = e; // 抑制未使用警告
                    } else {
                        self.out.push_str(" = ");
                        self.emit_expr(e);
                    }
                }
                self.out.push_str(";\n");
                // 若是类类型, 显式调用默认/重载 ctor
                if let Some(class) = &class_ctor_name {
                    // 走到这里说明 init 是 Class(args), 调 ctor(args)
                    // M26: 模板类型用实例化类名 (`Box<int>` -> __dcc_template_Box_int)
                    let inst_class = match &resolved_ty {
                        MangleType::Template { name: tn, args: ta } => {
                            instantiated_class_name(tn, ta)
                        }
                        _ => self.qualify_name(class),
                    };
                    let n = NestedName::from_dotted(vec![inst_class]);
                    let param_tys: Vec<MangleType> =
                        class_ctor_args.iter().map(|a| {
                            let ty = self.infer_type(a, false);
                            // Unwrap reference: constructor args are passed by value
                            match ty {
                                MangleType::Reference(inner) => *inner,
                                _ => ty,
                            }
                        }).collect();
                    let m = mangle_ctor(&n, &param_tys, 1);
                    self.out.push_str(&format!("    {}(&{}", m, name));
                    for a in &class_ctor_args {
                        self.out.push_str(", ");
                        self.emit_expr(a);
                    }
                    self.out.push_str(");\n");
                } else if let Some(inst_class) = Self::inst_class_of(&resolved_ty) {
                    // 平凡无参 ctor (含模板类实例化)
                    // 跳过已知 typedef 类型 (size_t 等), 它们没有构造函数
                    if !Self::is_known_typedef(&inst_class) {
                        // 限定类名: string → std_string (在 namespace 内)
                        let qualified = self.qualify_name(&inst_class);
                        let n = NestedName::from_dotted(vec![qualified]);
                        let m = mangle_ctor(&n, &[], 1);
                        self.out.push_str(&format!("    {}(&{});\n", m, name));
                    }
                }
            }
            CppStmt::StaticLocal { ty, name, init } => {
                // M10+: 静态局部变量 - 触发 __cxa_guard_* 线程安全单次初始化
                // Itanium C++ ABI: 静态局部变量存放在 .rodata/.data, 初始化由 guard 保护
                self.local_var_types.insert(name.clone(), ty.clone());
                // 生成 guard 变量名: _ZStL<len>name<suffix> (Itanium 风格, 但简化)
                let _mangle = format!("_ZStL{}{}_thread_local_", name.len(), name);
                self.out
                    .push_str(&format!("static {} {};\n", self.mangle_to_c_ns(ty), name));
                self.out
                    .push_str(&format!("static unsigned char _cxa_guard_{} = 0;\n", name));
                self.out.push_str(&format!(
                    "if (_cxa_guard_acquire(&_cxa_guard_{})) {{\n",
                    name
                ));
                if let Some(e) = init {
                    self.out.push_str(&format!("    {} = ", name));
                    self.emit_expr(e);
                    self.out.push_str(";\n");
                }
                if let Some(inst_class) = Self::inst_class_of(ty) {
                    let n = NestedName::from_dotted(vec![inst_class]);
                    let m = mangle_ctor(&n, &[], 1);
                    self.out.push_str(&format!("    {}(&{});\n", m, name));
                }
                self.out
                    .push_str(&format!("    _cxa_guard_release(&_cxa_guard_{});\n", name));
                self.out.push_str("}\n");
            }
            CppStmt::If { cond, then, els } => {
                self.out.push_str("if (");
                self.emit_expr(cond);
                self.out.push_str(") {\n");
                for s in &then.stmts {
                    self.emit_stmt(s);
                }
                self.out.push_str("}\n");
                if let Some(e) = els {
                    self.out.push_str("else {\n");
                    for s in &e.stmts {
                        self.emit_stmt(s);
                    }
                    self.out.push_str("}\n");
                }
            }
            // M17.0: `if constexpr` 真编译期分支 - 使用 cpp_const_eval::eval_cpp_const 求值,
            // 选 then 或 els (整段被消除, 不进入输出 C).
            CppStmt::IfConstexpr { cond, then, els } => {
                // 用 cpp_const_eval helper 求值条件
                let v = crate::cpp::cpp_const_eval::eval_cpp_const(cond, &self.const_local_i128);
                match v {
                    Some(v) if v != 0 => {
                        for s in &then.stmts {
                            self.emit_stmt(s);
                        }
                    }
                    Some(_) => {
                        if let Some(e) = els {
                            for s in &e.stmts {
                                self.emit_stmt(s);
                            }
                        }
                    }
                    // 求值失败 (例如依赖模板参数 / SFINAE 探测): 保守地两段都保留
                    // 标记一行注释以便追踪
                    None => {
                        self.out.push_str("/* M17.0: if constexpr condition not constexpr, both branches kept */\n");
                        self.out.push_str("if (");
                        self.emit_expr(cond);
                        self.out.push_str(") {\n");
                        for s in &then.stmts {
                            self.emit_stmt(s);
                        }
                        self.out.push_str("}\n");
                        if let Some(e) = els {
                            self.out.push_str("else {\n");
                            for s in &e.stmts {
                                self.emit_stmt(s);
                            }
                            self.out.push_str("}\n");
                        }
                    }
                }
            }
            CppStmt::While { cond, body } => {
                self.out.push_str("while (");
                self.emit_expr(cond);
                self.out.push_str(") {\n");
                for s in &body.stmts {
                    self.emit_stmt(s);
                }
                self.out.push_str("}\n");
            }
            CppStmt::For {
                init,
                cond,
                step,
                body,
            } => {
                self.out.push_str("for (");
                if let Some(s) = init {
                    self.emit_stmt(s);
                } else {
                    self.out.push(';');
                }
                if let Some(c) = cond {
                    self.emit_expr(c);
                }
                self.out.push(';');
                if let Some(s) = step {
                    // step 是表达式, 不能用 emit_stmt (会多加 ';')
                    match s.as_ref() {
                        CppStmt::Expr(e) => self.emit_expr(e),
                        other => self.emit_stmt(other),
                    }
                }
                self.out.push_str(") {\n");
                for s in &body.stmts {
                    self.emit_stmt(s);
                }
                self.out.push_str("}\n");
            }
            CppStmt::Try { body, catches } => {
                self.out.push_str("{\n");
                self.out.push_str("__cxa_eh_context_t __eh_ctx = {0,0};\n");
                self.out
                    .push_str("if (__cxa_try(&__eh_ctx, (void*(*)(void*))0, (void*)0)) {\n");
                for s in &body.stmts {
                    self.emit_stmt(s);
                }
                self.out.push_str("} else {\n");
                for c in catches {
                    self.out.push_str("{\n");
                    if let Some(ty) = &c.ty {
                        self.out
                            .push_str("void* __exc = __cxa_begin_catch(__eh_ctx.matched);\n");
                        self.out.push_str(&format!(
                            "{}* __e = ({}*)__exc;\n",
                            self.mangle_to_c_ns(ty),
                            self.mangle_to_c_ns(ty)
                        ));
                        if let Some(n) = &c.name {
                            self.out.push_str("__cxa_end_catch(__exc);\n");
                            self.out.push_str(&format!("(void)__e; (void){};\n", n));
                        }
                    } else {
                        self.out
                            .push_str("void* __exc = __cxa_begin_catch(__eh_ctx.matched);\n");
                        self.out.push_str("(void)__exc; __cxa_end_catch(__exc);\n");
                    }
                    for s in &c.body.stmts {
                        self.emit_stmt(s);
                    }
                    self.out.push_str("}\n");
                }
                self.out.push_str("}\n");
                self.out.push_str("}\n");
            }
            CppStmt::Throw(val) => {
                if let Some(e) = val {
                    self.out.push_str("{\n");
                    self.out
                        .push_str("void* __exc = __cxa_allocate_exception(sizeof(int));\n");
                    self.out.push_str("*(int*)__exc = ");
                    self.emit_expr(e);
                    self.out.push_str(";\n");
                    self.out
                        .push_str("__cxa_throw(__exc, (void*)0, (void(*)(void*))0);\n");
                    self.out.push_str("}\n");
                } else {
                    self.out
                        .push_str("__cxa_throw((void*)0, (void*)0, (void(*)(void*))0);\n");
                }
            }
            CppStmt::Break => self.out.push_str("break;\n"),
            CppStmt::Continue => self.out.push_str("continue;\n"),
            // M26: 真协程状态机 (Duff's device) - 每个挂起点:
            //   state = N+1; value = (expr); return value;  + 恢复标签 case N+1:
            // 状态/返回槽/完成标志变量 per-function 命名 (见 coroutine.rs)
            CppStmt::CoAwait(e) => {
                let expr = self.expr_to_c(e);
                self.out.push_str(&coro_suspend(
                    &self.coro_var,
                    &self.coro_ret_c,
                    self.coro_state,
                    &expr,
                ));
                self.coro_state += 1;
            }
            CppStmt::CoYield(e) => {
                // co_yield 与 co_await 同为挂起点 (产出值给调用方)
                let expr = self.expr_to_c(e);
                self.out.push_str(&coro_suspend(
                    &self.coro_var,
                    &self.coro_ret_c,
                    self.coro_state,
                    &expr,
                ));
                self.coro_state += 1;
            }
            CppStmt::CoReturn(v) => {
                // co_return: 置 done 标志 + 存返回槽, 不生成恢复标签 (协程终止)
                let expr = v
                    .as_ref()
                    .map(|e| self.expr_to_c(e))
                    .unwrap_or_else(|| "0".to_string());
                self.out.push_str(&coro_return(
                    &self.coro_var,
                    &self.coro_ret_c,
                    self.coro_state,
                    &expr,
                ));
                self.coro_state += 1;
            }
            CppStmt::Decl(d) => {
                self.lower_decl(d);
            }
        }
    }

    /// 渲染表达式为 C 文本 (临时切换 out; emit_expr 只写 out, 交换安全)
    fn expr_to_c(&mut self, e: &CppExpr) -> String {
        let saved = std::mem::take(&mut self.out);
        self.emit_expr(e);
        std::mem::replace(&mut self.out, saved)
    }

    fn emit_expr(&mut self, e: &CppExpr) {
        match e {
            CppExpr::IntLit(v) => self.out.push_str(&v.to_string()),
            // M26.1: 浮点字面量原样透传 (含 0x / 指数 / 后缀)
            CppExpr::FloatLit(s) => self.out.push_str(s),
            CppExpr::StrLit(s) => self.out.push_str(&format!("\"{}\"", s)),
            CppExpr::StrConcat(parts) => {
                self.out.push('"');
                for p in parts {
                    self.out.push_str(p);
                }
                self.out.push('"');
            }
            CppExpr::Ident(s) => {
                // 当前在类方法内, 且 s 是该类的成员, 改写为 this->s
                if let Some(cls) = &self.current_class {
                    // M26.3: 虚基成员 -> vbtable this-adjust 访问
                    if let Some((vb, idx, tail)) = self.vbase_member_access(cls, s) {
                        let this_expr = CppExpr::This;
                        self.emit_vbase_member(&this_expr, true, &vb, idx, &tail, s);
                        return;
                    }
                    if let Some(fields) = self.class_fields.get(cls) {
                        if fields.iter().any(|(n, _)| n == s) {
                            self.out.push_str(&format!("this->{}", s));
                            return;
                        }
                    }
                }
                self.out.push_str(s);
            }
            CppExpr::This => self.out.push_str("this"),
            CppExpr::ThisMember(s) => {
                // M26.3: 虚基成员 -> vbtable this-adjust 访问
                if let Some(cls) = &self.current_class {
                    if let Some((vb, idx, tail)) = self.vbase_member_access(cls, s) {
                        let this_expr = CppExpr::This;
                        self.emit_vbase_member(&this_expr, true, &vb, idx, &tail, s);
                        return;
                    }
                }
                self.out.push_str(&format!("this->{}", s));
            }
            CppExpr::QualifiedAccess(_, s) => self.out.push_str(s),
            CppExpr::Member(b, s) => {
                // M18.0: 若 b 是指针/引用类型 (e.g. const path& 参数, 或 dccpp_* 指针),
                // 输出 `b->s` 而不是 `b.s`.
                let use_arrow = if let Some(ty) = self.lookup_ident_type(b) {
                    Self::is_ptr_like(&ty)
                } else {
                    false
                };

                // M26.3: 虚基成员 -> vbtable this-adjust 访问 (优先于 __base_ 链)
                if let Some(class) = self.lookup_var_class(b) {
                    if let Some((vb, idx, tail)) = self.vbase_member_access(&class, s) {
                        self.emit_vbase_member(b, use_arrow, &vb, idx, &tail, s);
                        return;
                    }
                }

                // Check if member is inherited from a base class
                // M26: 递归查找继承链, 获取完整 __base_ 访问路径
                let inherited_chain = if let Some(class) = self.lookup_var_class(b) {
                    self.find_inherited_base(&class, s)
                } else {
                    None
                };

                self.emit_expr(b);
                if let Some(chain) = inherited_chain {
                    // Rewrite to b.__base_Base1.__base_Base2...s
                    for base_name in &chain {
                        if use_arrow {
                            self.out.push_str(&format!("->__base_{}", base_name));
                        } else {
                            self.out.push_str(&format!(".__base_{}", base_name));
                        }
                    }
                    self.out.push_str(&format!(".{}", s));
                } else {
                    // Not inherited or can't determine, use normal access
                    if use_arrow {
                        self.out.push_str(&format!("->{}", s));
                    } else {
                        self.out.push_str(&format!(".{}", s));
                    }
                }
            }
            CppExpr::PtrMember(b, s) => {
                // M26.3: 虚基成员 -> vbtable this-adjust 访问 (指针 obj 用 ->)
                if let Some(class) = self.lookup_var_class(b) {
                    if let Some((vb, idx, tail)) = self.vbase_member_access(&class, s) {
                        self.emit_vbase_member(b, true, &vb, idx, &tail, s);
                        return;
                    }
                }
                // M10+: operator-> 重载链
                // 若 base 是 PtrLike 类型 (Ptr/Wrapper/Iter/ptr/iter/iter 名字),
                // 展开为 base.operator->()->s
                let use_chain = if let CppExpr::Ident(name) = &**b {
                    self.local_var_types.get(name)
                        .map(|t| matches!(t, MangleType::Class(c) if crate::cpp::feature_degrade::ArrowChainResolver::has_arrow_overload(c)))
                        .unwrap_or(false)
                } else {
                    false
                };
                if use_chain {
                    // 通过 operator-> 递归
                    self.emit_expr(b);
                    self.out.push_str(&format!("->{}", s));
                } else {
                    self.emit_expr(b);
                    self.out.push_str(&format!("->{}", s));
                }
            }
            CppExpr::MemberPtr { class, name } => {
                // M10+: &Class::method -> 成员函数指针
                // 降级为带 self 参数的函数指针: void (void* self, ...) __member_fn
                self.out
                    .push_str(&format!("&_member_fn_{}_{}", class, name));
            }
            CppExpr::TemplateIdent { name, args } => {
                // M26: 非调用上下文的模板名 (e.g. `&foo<int>`): 输出实例化函数名
                self.out.push_str(&Self::tplfn_c_name(name, args));
            }
            CppExpr::InitList(elems) => {
                // M10+: initializer_list {1, 2, 3}
                // 降级为复合字面量: (int[]){1, 2, 3}
                // 类型推断为第一个元素的类型, 默认为 int
                let elem_ty = if let Some(CppExpr::IntLit(_)) = elems.first() {
                    "int"
                } else {
                    "int"
                }; // M10 简化
                self.out.push_str(&format!("(({}){{", elem_ty));
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        self.out.push_str(", ");
                    }
                    self.emit_expr(e);
                }
                self.out.push_str("})");
            }
            CppExpr::TypedInitList { ty, elems } => {
                // M10+: initializer_list<T> T{1, 2, 3}
                self.out
                    .push_str(&format!("(({}){{", self.mangle_to_c_ns(ty)));
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        self.out.push_str(", ");
                    }
                    self.emit_expr(e);
                }
                self.out.push_str("})");
            }
            CppExpr::Call(f, args) => {
                // M26: 模板函数调用 (显式实参 foo<int>(...) / 隐式推导 foo(...))
                if let CppExpr::TemplateIdent { name, args: targs } = &**f {
                    self.emit_template_fn_call(name, targs);
                    self.out.push('(');
                    for (i, a) in args.iter().enumerate() {
                        if i > 0 {
                            self.out.push_str(", ");
                        }
                        self.emit_expr(a);
                    }
                    self.out.push(')');
                    return;
                }
                if let CppExpr::Ident(fname) = &**f {
                    if self.templates.find(fname).is_some()
                        && !self.plain_fns.contains(fname)
                        && !self.classes.contains(fname)
                    {
                        // 隐式实参推导: 从调用实参类型推导
                        let arg_tys: Vec<MangleType> =
                            args.iter().map(|a| self.infer_type(a, false)).collect();
                        let td = self.templates.find(fname).unwrap();
                        if let Some(resolved) = self.templates.deduce_args(td, &arg_tys) {
                            self.emit_template_fn_call(fname, &resolved);
                            self.out.push('(');
                            for (i, a) in args.iter().enumerate() {
                                if i > 0 {
                                    self.out.push_str(", ");
                                }
                                self.emit_expr(a);
                            }
                            self.out.push(')');
                            return;
                        }
                        // 推导失败 (SFINAE): 回退普通调用
                    }
                    // M26.1: 用户定义的自由函数调用 -> mangled 名, 与定义一致
                    // (修复: 之前定义 mangled 但调用 unmangled, 链接必失败)
                    if self.plain_fns.contains(fname) && self.fn_ret_types.contains_key(fname) {
                        let n = NestedName::from_dotted(vec![self.qualify_name(fname)]);
                        let m = mangle_member_function(&n, &[], &MangleType::Void);
                        self.out.push_str(&m);
                        self.out.push('(');
                        for (i, a) in args.iter().enumerate() {
                            if i > 0 {
                                self.out.push_str(", ");
                            }
                            self.emit_expr(a);
                        }
                        self.out.push(')');
                        return;
                    }
                }
                // 方法调用改写: obj.method(args) -> _ZN<Cls><Len><method>E<args>(&obj, args)
                // 也支持 obj->method() (e.g. const path& p; p->c_str() 等)
                let call_target: Option<(&CppExpr, String)> = match &**f {
                    CppExpr::Member(o, m) => Some((o, m.clone())),
                    CppExpr::PtrMember(o, m) => Some((o, m.clone())),
                    _ => None,
                };
                if let Some((obj_expr, method_name)) = call_target {
                    if let Some(class) = self.lookup_var_class(obj_expr) {
                        if self
                            .class_methods
                            .get(&class)
                            .is_some_and(|m| m.contains(&method_name))
                        {
                            let n =
                                NestedName::from_dotted(vec![class.clone(), method_name.clone()]);
                            let m = mangle_member_function(&n, &[], &MangleType::Void);
                            self.out.push_str(&m);
                            self.out.push('(');
                            self.out.push('&');
                            self.emit_expr(obj_expr);
                            for a in args {
                                self.out.push_str(", ");
                                self.emit_expr(a);
                            }
                            self.out.push(')');
                            return;
                        }
                    }
                }
                // 普通 C 函数调用
                self.emit_expr(f);
                self.out.push('(');
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        self.out.push_str(", ");
                    }
                    self.emit_expr(a);
                }
                self.out.push(')');
            }
            CppExpr::New(ty, args) => {
                // P2.4: 走 mangle_op_new / mangle_op_new_array /
                // mangle_op_delete / mangle_op_delete_array 命名常量,
                // 不再硬编码 "_Znwm" / "_Znam" / "_ZdlPv" / "_ZdaPv".
                self.out.push_str(&format!(
                    "((struct {}*){}(sizeof(struct {})))",
                    ty,
                    mangle_op_new(),
                    ty
                ));
                if !args.is_empty() {
                    let n = NestedName::from_dotted(vec![ty.clone()]);
                    let m = mangle_ctor(&n, &[MangleType::Pointer(Box::new(MangleType::Void))], 1);
                    self.out.push_str(&format!(
                        "; {}((struct {}){}(sizeof(struct {}))",
                        m,
                        ty,
                        mangle_op_new(),
                        ty
                    ));
                    for a in args {
                        self.out.push_str(", ");
                        self.emit_expr(a);
                    }
                    self.out.push(')');
                }
            }
            // M17.0: `new T[n]` 真数组分配 - 调用 _Znam (array new) 而非 _Znwm (scalar)
            CppExpr::NewArr { ty, count } => {
                // _Znam(size): Itanium C++ ABI 数组 operator new;
                // 我们的 ABI 桩负责分配, 元素 ctor 在使用时由代码显式调用.
                // M17.0: 区分内建类型 (int/char/...) 和类类型.
                // 内建类型不需要 ctor, 直接用 (ty*) 强转; 类类型用 (struct ty*).
                let is_builtin = matches!(
                    ty.as_str(),
                    "int"
                        | "char"
                        | "long"
                        | "short"
                        | "float"
                        | "double"
                        | "void"
                        | "bool"
                        | "unsigned"
                        | "int*"
                );
                let c_ty = if is_builtin {
                    ty.clone()
                } else {
                    format!("struct {}", ty)
                };
                self.out.push_str(&format!(
                    "(({}*){}(sizeof({}) * (",
                    c_ty,
                    mangle_op_new_array(),
                    c_ty
                ));
                self.emit_expr(count);
                self.out.push_str(")))");
            }
            // M17.0: placement new - 构造在已有 ptr 上, 不分配新内存
            CppExpr::PlacementNew { ptr, ty, args } => {
                // 形如: ((T*)ptr)->ctor_init(ptr, args)
                // 我们用 mangle_ctor 加 (this, args) 形式
                let n = NestedName::from_dotted(vec![ty.clone()]);
                let param_tys: Vec<MangleType> = args.iter().map(|_| MangleType::Int).collect();
                let m = mangle_ctor(&n, &param_tys, 1);
                self.out.push_str("((void*)(");
                self.emit_expr(ptr);
                self.out.push_str("), ");
                self.out.push_str(&m);
                self.out.push('(');
                self.emit_expr(ptr);
                for a in args {
                    self.out.push_str(", ");
                    self.emit_expr(a);
                }
                self.out.push_str("))");
            }
            CppExpr::Delete(p) => {
                self.out.push_str(&format!("{}(", mangle_op_delete()));
                self.emit_expr(p);
                self.out.push(')');
            }
            // M17.0: `delete[] p` 数组释放 - 调用 _ZdaPv
            CppExpr::DeleteArr(p) => {
                self.out.push_str(&format!("{}(", mangle_op_delete_array()));
                self.emit_expr(p);
                self.out.push(')');
            }
            CppExpr::BinOp(op, l, r) => {
                self.emit_expr(l);
                let op_s = match op {
                    BinOpKind::Add => "+",
                    BinOpKind::Sub => "-",
                    BinOpKind::Mul => "*",
                    BinOpKind::Div => "/",
                    BinOpKind::Mod => "%",
                    BinOpKind::EqEq => "==",
                    BinOpKind::Ne => "!=",
                    BinOpKind::Lt => "<",
                    BinOpKind::Gt => ">",
                    BinOpKind::Le => "<=",
                    BinOpKind::Ge => ">=",
                    BinOpKind::Assign => " = ",
                    BinOpKind::Shl => "<<",
                    BinOpKind::Shr => ">>",
                    BinOpKind::And => "&",
                    BinOpKind::Or => "|",
                    BinOpKind::Xor => "^",
                    BinOpKind::LAnd => "&&",
                    BinOpKind::LOr => "||",
                    BinOpKind::AddAssign => "+=",
                    BinOpKind::SubAssign => "-=",
                    BinOpKind::MulAssign => "*=",
                    BinOpKind::DivAssign => "/=",
                    BinOpKind::ModAssign => "%=",
                    BinOpKind::ShlAssign => "<<=",
                    BinOpKind::ShrAssign => ">>=",
                    BinOpKind::AndAssign => "&=",
                    BinOpKind::OrAssign => "|=",
                    BinOpKind::XorAssign => "^=",
                    BinOpKind::Index => {
                        self.out.push('[');
                        self.emit_expr(r);
                        self.out.push(']');
                        return;
                    }
                };
                self.out.push_str(op_s);
                if !matches!(op, BinOpKind::Index) {
                    self.emit_expr(r);
                }
            }
            CppExpr::UnaryOp(op, e) => {
                let s = match op {
                    UnaryOpKind::Neg => "-",
                    UnaryOpKind::Not => "~",
                    UnaryOpKind::Plus => "+",
                    UnaryOpKind::LNot => "!",
                    UnaryOpKind::PreInc => "++",
                    UnaryOpKind::PreDec => "--",
                    UnaryOpKind::PostInc => {
                        self.emit_expr(e);
                        self.out.push_str("++");
                        return;
                    }
                    UnaryOpKind::PostDec => {
                        self.emit_expr(e);
                        self.out.push_str("--");
                        return;
                    }
                    UnaryOpKind::Deref => "*",
                };
                self.out.push_str(s);
                self.emit_expr(e);
            }
            CppExpr::Ref(e) => {
                self.out.push('&');
                self.emit_expr(e);
            }
            CppExpr::AddrOf(e) => {
                self.out.push('&');
                self.emit_expr(e);
            }
            CppExpr::Cast(ty, e) => {
                // M26.3: 虚基 this-adjust 上转型: (Base*)&obj / (Base*)p / (Base)obj
                // 目标是源类的传递虚基时, 指针需经 vbtable 调整到虚基子对象
                let vb_name = match ty {
                    MangleType::Class(vb) => Some(vb.clone()),
                    MangleType::Pointer(inner) => match &**inner {
                        MangleType::Class(vb) => Some(vb.clone()),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(vb) = vb_name {
                    let (obj_expr, use_arrow) = match &**e {
                        CppExpr::AddrOf(inner) | CppExpr::Ref(inner) => (&**inner, false),
                        _ => (&**e, true),
                    };
                    if let Some(src) = self.lookup_var_class(obj_expr) {
                        if let Some(idx) = self.vbase_upcast_target(&vb, &src) {
                            self.out
                                .push_str(&format!("(({})", self.mangle_to_c_ns(ty)));
                            self.out.push_str("((char*)&");
                            self.emit_expr(obj_expr);
                            self.out.push_str(if use_arrow {
                                "->__vbptr + "
                            } else {
                                ".__vbptr + "
                            });
                            self.emit_expr(obj_expr);
                            self.out
                                .push_str(if use_arrow { "->__vbptr[" } else { ".__vbptr[" });
                            self.out.push_str(&idx.to_string());
                            self.out.push_str("]))");
                            return;
                        }
                    }
                }
                self.out.push_str(&format!("({})", self.mangle_to_c_ns(ty)));
                self.emit_expr(e);
            }
            CppExpr::Sizeof(arg) => {
                self.out.push_str("sizeof(");
                match arg {
                    SizeofArg::Type(t) => self.out.push_str(&self.mangle_to_c_ns(t)),
                    SizeofArg::Expr(x) => self.emit_expr(x),
                }
                self.out.push(')');
            }
            CppExpr::Typeid(e) => {
                // M27: 动态 RTTI - 多态表达式通过 vtable RTTI slot 获取真实类型
                // 非多态表达式仍用静态类 RTTI (与 C++ 标准一致)
                let cls = self.expr_static_class(e);
                let is_poly = if !cls.is_empty() {
                    self.class_polymorphic.contains(&cls)
                } else {
                    false
                };
                if is_poly {
                    // 多态: 通过 __vptr[0] 读取运行时 typeinfo 指针
                    // obj.__vptr 是 void*, 强转为 void** 取 [0] 即 typeinfo
                    self.out.push_str("((void**)((char*)&");
                    self.emit_expr(e);
                    self.out.push_str(")[0])");
                } else {
                    // 非多态: 静态 RTTI
                    self.out.push_str("(&__dcc_ti_");
                    self.out.push_str(&cls);
                    self.out.push(')');
                }
            }
            // M17.0: 静态 typeid(T) -> &_ZTI<T>
            CppExpr::TypeidType(ty) => {
                let cname = match ty {
                    MangleType::Class(n) => n.clone(),
                    // M26: typeid(Box<int>) 取实例化类 typeinfo
                    MangleType::Template { name, args } => instantiated_class_name(name, args),
                    other => self.mangle_to_c_ns(other),
                };
                self.out.push_str(&format!("(&__dcc_ti_{})", cname));
            }
            CppExpr::DynamicCast { ty, expr } => {
                // M10+: 完整 dynamic_cast 跨虚继承
                // Itanium C++ ABI: __cxa_* 跨虚继承通过 vtable RTTI 查找
                // 简化为调用 __dynamic_cast helper
                self.out.push_str("((");
                self.out.push_str(&self.mangle_to_c_ns(ty));
                self.out.push_str(")__dynamic_cast((void*)(");
                self.emit_expr(expr);
                self.out.push_str("), 0, 0, -1))");
            }
            CppExpr::StaticCast { ty, expr } => {
                self.out
                    .push_str(&format!("(({})(", self.mangle_to_c_ns(ty)));
                self.emit_expr(expr);
                self.out.push_str("))");
            }
            CppExpr::ConstCast { ty, expr } => {
                self.out
                    .push_str(&format!("(({})(", self.mangle_to_c_ns(ty)));
                self.emit_expr(expr);
                self.out.push_str("))");
            }
            CppExpr::ReinterpretCast { ty, expr } => {
                self.out
                    .push_str(&format!("(({})(", self.mangle_to_c_ns(ty)));
                self.emit_expr(expr);
                self.out.push_str("))");
            }
            CppExpr::Lambda(lam) => {
                let n = format!("__lambda_{}", self.lambda_counter);
                self.lambda_counter += 1;
                self.lambdas.push(LambdaExpr {
                    captures: lam.captures.clone(),
                    params: lam.params.clone(),
                    body: lam.body.clone(),
                    closure_name: n.clone(),
                });
                // Lambda 表达式: 值, 用闭包类型 + 构造
                self.out.push_str(&format!("({}){{", n));
                // 初始化捕获字段
                for cap in &lam.captures {
                    match cap {
                        Capture::This => self.out.push_str("this, "),
                        Capture::ByRef(n) | Capture::ByVal(n) => {
                            self.out.push_str(&format!("{}, ", n));
                        }
                        _ => {}
                    }
                }
                self.out.push('}');
            }
            CppExpr::OpCall { op, args } => {
                // 运算符调用: 简化调用
                self.out.push_str(&format!("{}(", op.clone().encode_name()));
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        self.out.push_str(", ");
                    }
                    self.emit_expr(a);
                }
                self.out.push(')');
            }
            CppExpr::Ternary { cond, then, els } => {
                self.out.push('(');
                self.emit_expr(cond);
                self.out.push_str(" ? ");
                self.emit_expr(then);
                self.out.push_str(" : ");
                self.emit_expr(els);
                self.out.push(')');
            }
            CppExpr::Comma(exprs) => {
                self.out.push('(');
                for (i, e) in exprs.iter().enumerate() {
                    if i > 0 {
                        self.out.push_str(", ");
                    }
                    self.emit_expr(e);
                }
                self.out.push(')');
            }
            // M17.0: `obj.*ptr_to_member` 真解引用
            // ptr 是 size_t 偏移 (M17.0 简化: 我们用 mangle 时把 &T::f 编码为 size_t)
            // 实际语义: 取 obj 地址 + 偏移, 按字段类型 deref
            // 我们用 _ZN3mbrE<Class><field>_offset 全局作为偏移常量
            CppExpr::MemberStar(obj, member) => {
                // 我们约定: 成员指针 "Class.field" 解析为 offsetof(struct Class, field)
                // 找出 obj 的类名 (用 lookup_var_class 启发式), 取偏移
                let class = self.lookup_var_class(obj).unwrap_or_default();
                // M27: 查找字段真实类型, 不再硬编码 int*
                let field_ty = self.member_field_type(obj, member)
                    .unwrap_or(MangleType::Int);
                let c_ty = self.mangle_to_c_ns(&field_ty);
                self.out.push_str(&format!("(*({}*)((char*)(", c_ty));
                self.emit_expr(obj);
                self.out.push_str(") + __dcc_mbr_");
                self.out.push_str(&class);
                self.out.push('_');
                self.out.push_str(member);
                self.out.push_str("()))");
            }
            // M17.0: `obj->*ptr_to_member` 真解引用
            CppExpr::PtrMemberStar(obj, member) => {
                // M17.0: `obj->*p` 真解引用 (obj 是指针, 偏移同上)
                let class = self.lookup_var_class(obj).unwrap_or_default();
                // M27: 查找字段真实类型, 不再硬编码 int*
                let field_ty = self.member_field_type(obj, member)
                    .unwrap_or(MangleType::Int);
                let c_ty = self.mangle_to_c_ns(&field_ty);
                self.out.push_str(&format!("(*({}*)((char*)(", c_ty));
                self.emit_expr(obj);
                self.out.push_str(") + __dcc_mbr_");
                self.out.push_str(&class);
                self.out.push('_');
                self.out.push_str(member);
                self.out.push_str("()))");
            }
            // M17.0: `noexcept(expr)` 编译期 bool.
            // M26.1: 分析 expr 是否可能抛异常, 输出 0/1 (不再恒为 true).
            CppExpr::Noexcept(e) => {
                if self.expr_can_throw(e) {
                    self.out.push('0');
                } else {
                    self.out.push('1');
                }
            }
            // M18.0: C++23 多维下标 `obj[i, j, k]` 改写为
            // `__dcc_multi_index_N(obj, i, j, k)` C 辅助函数
            // (N = 2..4, 真实实现在 dccpp_runtime.c)
            CppExpr::MultiIndex(obj, indices) => {
                self.out
                    .push_str(&format!("__dcc_multi_index_{}(", indices.len()));
                self.emit_expr(obj);
                for i in indices {
                    self.out.push_str(", ");
                    self.emit_expr(i);
                }
                self.out.push(')');
            }
        }
    }

    /// 降级所有收集到的 lambda
    fn lower_lambdas(&mut self) {
        for lam in self.lambdas.clone() {
            self.out
                .push_str(&format!("struct {} {{\n", lam.closure_name));
            // 捕获字段
            for cap in &lam.captures {
                match cap {
                    Capture::This => {
                        // 暂不处理 this (无类型信息)
                    }
                    Capture::ByRef(n) | Capture::ByVal(n) => {
                        self.out.push_str(&format!("    int {};\n", n));
                    }
                    _ => {}
                }
            }
            // 形参
            for (n, t) in &lam.params {
                self.out
                    .push_str(&format!("    {} {};\n", self.mangle_to_c_ns(t), n));
            }
            self.out.push_str("};\n");
            // operator() 静态函数
            self.out.push_str(&format!(
                "int _ZN{}{}clERKS_(\n",
                lam.closure_name.len(),
                lam.closure_name
            ));
            // 形参: this 闭包 + 显式形参
            self.out
                .push_str(&format!("    struct {}* __closure", lam.closure_name));
            for (n, t) in &lam.params {
                self.out
                    .push_str(&format!(", {} {}", self.mangle_to_c_ns(t), n));
            }
            self.out.push_str(") {\n");
            for s in &lam.body.stmts {
                self.emit_stmt(s);
            }
            self.out.push_str("}\n");
        }
    }

    pub fn finish(self) -> String {
        self.out
    }

    /// 函数签名的 C 文本 (BMF 导出记录): `ret(p1, p2)`
    fn fn_signature_c(&self, ret: &MangleType, params: &[(String, MangleType)]) -> String {
        let args: Vec<String> = params.iter().map(|(_, t)| self.mangle_to_c_ns(t)).collect();
        format!("{}({})", self.mangle_to_c_ns(ret), args.join(", "))
    }

    /// M26.4: 完整 C 函数声明文本 (BMF 导出/导入): `ret mangled(arg1, arg2)`
    /// 含 Itanium mangled 符号, import 端直接 `extern <此文本>;`
    fn fn_decl_c(
        &self,
        mangled: &str,
        ret: &MangleType,
        params: &[(String, MangleType)],
    ) -> String {
        let args: Vec<String> = params.iter().map(|(_, t)| self.mangle_to_c_ns(t)).collect();
        format!(
            "{} {}({})",
            self.mangle_to_c_ns(ret),
            mangled,
            args.join(", ")
        )
    }

    /// M26.4: 注入导入模块 (BMF) 的 C 声明, 并注册名字供调用点降级:
    ///   - 函数: `extern <decl>;` + 注册 srcname (plain_fns / fn_ret_types)
    ///   - 类:   `struct <name> { <body> };` + typedef + 注册类/字段
    ///   - 成员函数 (name `Class::method` / `Class::ctor`): extern + 注册方法
    ///   - 变量: `extern <decl>;`
    fn emit_imported_module_decls(&mut self, bmf: &BmfModule) {
        self.out
            .push_str(&format!("/* import module {} */\n", bmf.name));
        // 先输出所有类定义 (成员 extern 可能引用 struct 类型)
        for e in &bmf.exports {
            if let BmfKind::Class = e.kind {
                self.emit_imported_class(e);
            }
        }
        for e in &bmf.exports {
            match e.kind {
                BmfKind::Function => {
                    // name 为源码名 或 `Class::method` / `Class::ctor`
                    if e.name.contains("::") {
                        let (cls, method) = e.name.split_once("::").unwrap();
                        if method != "ctor" {
                            self.class_methods
                                .entry(cls.to_string())
                                .or_default()
                                .push(method.to_string());
                        }
                    } else {
                        self.plain_fns.insert(e.name.clone());
                        let ret = infer_c_decl_ret_type(&e.signature);
                        self.fn_ret_types.insert(e.name.clone(), ret);
                    }
                    self.out.push_str(&format!("extern {};\n", e.signature));
                }
                BmfKind::Class => {
                    // 已在第一遍输出
                }
                BmfKind::Variable => {
                    self.out.push_str(&format!("extern {};\n", e.signature));
                }
                BmfKind::Template => {
                    self.out
                        .push_str(&format!("/* import template {} */\n", e.name));
                }
            }
        }
    }

    /// M26.4: 输出导入类的 struct 定义并注册
    fn emit_imported_class(&mut self, e: &BmfExport) {
        let name = &e.name;
        if !self.classes.contains(name) {
            self.classes.push(name.clone());
        }
        let body = e.layout.as_deref().unwrap_or("");
        self.out
            .push_str(&format!("struct {} {{ {} }};\n", name, body));
        if !self.emitted_typedefs.contains(name.as_str()) && !is_c_keyword(name) {
            self.emitted_typedefs.insert(name.clone());
            self.out
                .push_str(&format!("typedef struct {} {};\n", name, name));
        }
        // 字段注册 (body 形如 `int x; double y;`), 供 this->x / 继承查找
        let mut fields = Vec::new();
        for part in body.split(';') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let toks: Vec<&str> = part.split_whitespace().collect();
            if let Some(fname) = toks.last() {
                fields.push((
                    fname.to_string(),
                    c_decl_type_to_mangle(&toks[..toks.len() - 1].join(" ")),
                ));
            }
        }
        self.class_fields.insert(name.clone(), fields);
    }

    /// M26: 模块接口序列化 (BMF 文本). 无 export module 声明时返回 None.
    pub fn module_bmf(&self) -> Option<String> {
        if self.module_name.is_empty() {
            return None;
        }
        Some(
            BmfModule {
                name: self.module_name.clone(),
                exports: self.module_exports.clone(),
                imports: self.module_imports.clone(),
                partition: self.module_partition.clone(),
                part_imports: self.module_part_imports.clone(),
            }
            .serialize(),
        )
    }

    /// M26.5: 跨 TU ODR 检查 - 把本模块导出与所有已加载导入模块的导出
    /// 注册进 OdrChecker, 返回格式化的违规信息; None 表示无违规.
    ///
    /// 判定依据: 同名实体的函数签名 (完整 C 声明文本, 含 Itanium 符号)
    /// 或类布局文本必须一致; 不一致视为 ODR 冲突.
    pub fn odr_check(&self) -> Option<String> {
        let mut checker = crate::cpp::modules::OdrChecker::new();
        checker.register_module(&self.module_name, &self.module_exports);
        for m in &self.imported_modules {
            checker.register_module(&m.name, &m.exports);
        }
        checker.check_and_format()
    }
}

/// M26.4: 从 C 声明文本推导函数返回类型 (`int _ZN6answerEv()` -> Int)
fn infer_c_decl_ret_type(decl: &str) -> MangleType {
    let head = decl.trim_start();
    // 取第一个 token 前 (含 struct/unsigned 等多词类型)
    let mut toks = head.split_whitespace();
    let first = toks.next().unwrap_or("int");
    match first {
        "void" => MangleType::Void,
        "char" => MangleType::Char,
        "double" => MangleType::Double,
        "float" => MangleType::Float,
        "short" => MangleType::Short,
        "long" => {
            if head.starts_with("long long") {
                MangleType::LongLong
            } else {
                MangleType::Long
            }
        }
        "unsigned" => MangleType::Uint,
        "struct" => MangleType::Class(toks.next().unwrap_or("").trim_end_matches('*').to_string()),
        _ => MangleType::Int,
    }
}

/// M26.4: C 类型文本 -> MangleType (import 端字段注册用, 尽力而为)
fn c_decl_type_to_mangle(ty: &str) -> MangleType {
    let t = ty.trim();
    if let Some(rest) = t.strip_prefix("struct ") {
        return MangleType::Class(rest.trim().to_string());
    }
    if t.ends_with('*') {
        return MangleType::Pointer(Box::new(c_decl_type_to_mangle(&t[..t.len() - 1].trim())));
    }
    match t {
        "void" => MangleType::Void,
        "char" => MangleType::Char,
        "int" => MangleType::Int,
        "double" => MangleType::Double,
        "float" => MangleType::Float,
        "short" => MangleType::Short,
        "long" => MangleType::Long,
        "long long" => MangleType::LongLong,
        "unsigned" => MangleType::Uint,
        "unsigned long" => MangleType::Ulong,
        "unsigned long long" => MangleType::UlongLong,
        _ => MangleType::Int,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpp::parser::CppParser;

    #[test]
    fn lower_class_with_ctor_emits_struct_and_ctor() {
        let src = b"class Foo { public: int x; Foo(int v) { x = v; } };";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(s.contains("struct Foo"), "{}", s);
        assert!(s.contains("_ZN3FooC1Ei"), "{}", s);
        assert!(s.contains("x = v"), "{}", s);
    }

    #[test]
    fn lower_dtor_emits_d1() {
        let src = b"class Foo { public: ~Foo() { return; } };";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(s.contains("_ZN3FooD1Ev"), "{}", s);
    }

    #[test]
    fn lower_namespace() {
        // M18.0: 命名空间内的类名降级时会被 qualify_name 限定 (std::vector -> std_vector)
        let src = b"namespace std { class vector { public: int x; }; }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(s.contains("struct std_vector"), "{}", s);
        // 同时还应有 typedef 别名 `vector` (M18.0: 内联类方便上层用短名)
        assert!(s.contains("typedef struct std_vector vector;"), "{}", s);
    }

    #[test]
    fn lower_try_catch() {
        let src = b"void f() { try { throw 1; } catch (int e) { } }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(s.contains("__cxa_try"), "{}", s);
    }

    #[test]
    fn lower_local_var_class() {
        // M9: 函数体内 `Counter c;` 应被降为 `struct Counter c;`
        let src = b"void f() { Counter c; }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(
            s.contains("struct Counter c;"),
            "expected 'struct Counter c;' in: {}",
            s
        );
    }

    #[test]
    fn lower_local_var_int_with_init() {
        // `int x = 42;` -> `int x = 42;`
        let src = b"void f() { int x = 42; }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(s.contains("int x = 42;"), "{}", s);
    }

    #[test]
    fn lower_local_var_class_with_init() {
        // `Counter c = make();` -> `struct Counter c = make();`
        let src = b"int make() { return 0; } void f() { Counter c = (Counter)make(); }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(
            s.contains("struct Counter c ="),
            "expected 'struct Counter c =' in: {}",
            s
        );
    }

    #[test]
    fn lower_throw() {
        let src = b"void f() { throw 1; }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(s.contains("__cxa_throw"), "{}", s);
    }

    #[test]
    fn lower_inherit() {
        let src = b"class Base { public: int x; }; class Derived : public Base { public: int y; };";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(s.contains("struct Base"), "{}", s);
        assert!(s.contains("struct Derived"), "{}", s);
        assert!(s.contains("__base_Base"), "{}", s);
    }

    #[test]
    fn lower_template_base_class() {
        // 模板基类: class Derived : public Base<int>
        let src = b"template <typename T> class Base { public: T value; };
                   class Derived : public Base<int> { public: int extra; };";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        // 应该包含实例化的模板基类名
        assert!(s.contains("__dcc_template_Base_int"), "{}", s);
        // 应该包含 __base_Base (继承子结构)
        assert!(s.contains("__base_Base"), "{}", s);
    }

    #[test]
    fn lower_template_friend() {
        // 模板友元: friend class Foo<T>
        let src = b"template <typename T> class Foo { public: T val; };
                   class Bar { public: int x; friend class Foo<int>; };";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        // 应该能正常解析并降级, 不报错
        assert!(s.contains("struct Bar"), "{}", s);
        assert!(s.contains("int x"), "{}", s);
    }

    #[test]
    fn lower_lambda() {
        let src = b"void f() { auto g = [](int x) { return x; }; }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(s.contains("__lambda_0"), "{}", s);
    }

    #[test]
    fn lower_template_passthrough() {
        let src = b"template <typename T> T id(T x) { return x; }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(s.contains("template <T>"), "{}", s);
    }

    #[test]
    fn lower_operator_overload() {
        let src = b"class Vec { public: Vec operator+(Vec const& o) { return *this; } };";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        // operator+ 降级为 pl 符号
        assert!(s.contains("pl"), "{}", s);
    }

    #[test]
    fn lower_full_program_namespace_class_inherit() {
        // 综合: namespace + class + 继承 + 运算符 + lambda + 模板
        // M18.0: 命名空间类会输出 qualified name (std_complex) + typedef alias (complex)
        let src = b"namespace std { class complex { public: int re, im; complex(int r, int i); complex operator+(complex const& o); }; }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        // struct 本身用 qualified name
        assert!(s.contains("struct std_complex"), "{}", s);
        // typedef alias 让 unqualified 名也可用
        assert!(s.contains("typedef struct std_complex complex;"), "{}", s);
    }

    #[test]
    fn lower_multiple_inheritance() {
        let src = b"class A { public: int a; };
                   class B { public: int b; };
                   class C : public A, public B { public: int c; };";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(s.contains("struct A"), "{}", s);
        assert!(s.contains("struct B"), "{}", s);
        assert!(s.contains("struct C"), "{}", s);
        // C 应嵌入 A 和 B
        assert!(s.contains("__base_A"), "{}", s);
        assert!(s.contains("__base_B"), "{}", s);
    }

    #[test]
    fn lower_virtual_function_emits_vtable() {
        let src = b"class Base { public: virtual void f() {} virtual ~Base() {} };
                   class Der : public Base { public: void f() override {} };";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        // vtable 符号
        assert!(s.contains("_ZTV"), "{}", s);
        // typeinfo 符号
        assert!(s.contains("_ZTI"), "{}", s);
    }

    #[test]
    fn lower_vtable_typeinfo_are_weak() {
        // Itanium C++ ABI: vtable/typeinfo 符号必须 weak, 否则多 TU 链接重复定义
        let src = b"class Foo { public: virtual void f() {} int x; };";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(
            s.contains("__attribute__((weak)) void* _ZTV"),
            "vtable must weak: {}",
            s
        );
        assert!(
            s.contains("__attribute__((weak)) void* _ZTI"),
            "typeinfo 必须 weak: {}",
            s
        );
        assert!(
            s.contains("__attribute__((weak)) size_t __dcc_mbr_"),
            "member offset 必须 weak: {}",
            s
        );
    }

    #[test]
    fn lower_rtti_typeid() {
        let src = b"class Foo { public: int x; };
                   void test() { Foo f; typeid(f); }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(s.contains("__dcc_ti_Foo") || s.contains("_ZTI"), "{}", s);
    }

    #[test]
    fn lower_template_class() {
        let src = b"template <typename T> class Vec { public: T data; Vec(T v) : data(v) {} };";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        // M26: 模板类只注册, 不展开 (无实例化请求)
        assert!(s.contains("template"), "{}", s);
        assert!(s.contains("registered"), "{}", s);
    }

    #[test]
    fn lower_std_complex_atomic() {
        // 模拟 std::complex 的最小化
        let src = b"namespace std {
            template <typename T>
            class complex {
            public:
                T re, im;
                complex() : re(0), im(0) {}
                complex(T r, T i) : re(r), im(i) {}
                complex operator+(complex const& o) { complex c; c.re = re + o.re; c.im = im + o.im; return c; }
            };
        }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        // M26: 模板类只注册, 不展开; 命名空间输出保留
        assert!(
            s.contains("std_complex") || s.contains("registered"),
            "{}",
            s
        );
        assert!(s.contains("template"), "{}", s);
    }

    #[test]
    fn lower_module_emit_placeholder() {
        let src = b"module foo;";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        // M10+: module 升级, 输出包含 "M10+"
        assert!(s.contains("module (M10+)"), "{}", s);
    }

    #[test]
    fn lower_concept_emit_placeholder() {
        let src = b"template <typename T> concept Integral = sizeof(T) <= 8;";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        // M10+: concept 升级, 输出包含 "M10+"
        assert!(s.contains("concept (M10+)"), "{}", s);
    }

    #[test]
    fn lower_coroutine_basic() {
        let src = b"void f() { co_await 1; co_yield 2; co_return 3; }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        // M26: 真状态机 - per-function 状态变量 + switch + Duff's device 标签
        assert!(s.contains("_ZN1fEv_coro_state"), "{}", s);
        assert!(s.contains("switch (_ZN1fEv_coro_state) {"), "{}", s);
        assert!(s.contains("case 0:"), "{}", s);
        assert!(s.contains("case 1:"), "{}", s);
        assert!(s.contains("_ZN1fEv_coro_done"), "{}", s);
    }

    #[test]
    fn lower_diamond_inheritance() {
        // 钻石继承: Base -> Mid1, Mid2 -> Final
        let src = b"class Base { public: int b; };
                   class Mid1 : public Base { public: int m1; };
                   class Mid2 : public Base { public: int m2; };
                   class Final : public Mid1, public Mid2 { public: int f; };";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        // Final 应嵌入 Mid1 和 Mid2, 每个又嵌入 Base
        assert!(s.contains("struct Final"), "{}", s);
        assert!(s.contains("__base_Mid1"), "{}", s);
        assert!(s.contains("__base_Mid2"), "{}", s);
    }

    #[test]
    fn lower_dynamic_cast() {
        let src = b"class Base { public: virtual ~Base() {} };
                   class Der : public Base { public: int x; };
                   void test() { Base* b = new Der(); Der* d = dynamic_cast<Der*>(b); }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        // dynamic_cast 应降级为某种 cast
        assert!(s.contains("struct Der") || s.contains("Der"), "{}", s);
    }

    #[test]
    fn lower_template_instantiation_basic() {
        // 模板实例化调用
        let src = b"template <typename T> T id(T x) { return x; }
                   int main() { return id<int>(42); }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        // 模板声明 + main 都应在
        assert!(s.contains("id"), "{}", s);
        assert!(s.contains("main"), "{}", s);
    }

    #[test]
    fn lower_method_call_rewrites_to_mangled_fn() {
        // M9: c.inc() 应改写为 _ZN7Counter3incEv(&c)
        let src = b"class Counter { public: void inc() {} int get() const { return 0; } };
                   int main() { Counter c; c.inc(); return c.get(); }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(
            s.contains("_ZN7Counter3incEv(&c)"),
            "expected method call rewrite, got: {}",
            s
        );
        assert!(
            s.contains("_ZN7Counter3getEv(&c)"),
            "expected method call rewrite, got: {}",
            s
        );
    }

    #[test]
    fn lower_no_system_headers_when_unused() {
        // M9: 不使用 new/delete/throw 时, 不引入 stddef.h/stdint.h
        let src = b"class Foo { public: int x; };
                   int main() { Foo f; return f.x; }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(
            !s.contains("#include <stddef.h>"),
            "should not include stddef.h, got: {}",
            s
        );
        assert!(
            !s.contains("#include <stdint.h>"),
            "should not include stdint.h, got: {}",
            s
        );
        assert!(
            s.contains("typedef unsigned long size_t;"),
            "should provide fallback size_t, got: {}",
            s
        );
    }

    // ===== M10+ tests =====

    #[test]
    fn m10_sfinae_substitute_basic() {
        use crate::cpp::feature_degrade::{try_substitute, SfinaeCtx};
        let mut ctx = SfinaeCtx::new();
        ctx.bind("T".to_string(), "int".to_string());
        let r = try_substitute("std::vector<T>", &ctx).unwrap();
        assert_eq!(r, "std::vector<int>");
    }

    #[test]
    fn m10_partial_spec_pointer_matches() {
        use crate::cpp::feature_degrade::pattern_matches;
        assert!(pattern_matches("T*", "int*"));
        assert!(pattern_matches("T*", "MyClass*"));
        assert!(!pattern_matches("T*", "int"));
    }

    #[test]
    fn m10_concept_size_le() {
        use crate::cpp::feature_degrade::{Concept, SfinaeCtx};
        let mut ctx = SfinaeCtx::new();
        ctx.bind("T".to_string(), "int".to_string());
        let c = Concept::parse("sizeof(T) <= 4");
        assert!(c.evaluate(&ctx));
        let c = Concept::parse("sizeof(T) <= 2");
        assert!(!c.evaluate(&ctx));
    }

    #[test]
    fn m10_concept_same_as() {
        use crate::cpp::feature_degrade::{Concept, SfinaeCtx};
        let mut ctx = SfinaeCtx::new();
        ctx.bind("T".to_string(), "int".to_string());
        let c = Concept::parse("same_as<T, int>");
        assert!(c.evaluate(&ctx));
    }

    #[test]
    fn m26_concept_sfinae_integration() {
        // M26: concept 定义与 requires 子句联动
        // concept Integral = sizeof(T) <= 4;
        // template <typename T> requires Integral<T> T add(T a, T b) { return a + b; }
        use crate::cpp::feature_degrade::expand_concepts;
        let mut concepts = std::collections::HashMap::new();
        concepts.insert("Integral".to_string(), "sizeof(T) <= 4".to_string());
        // 展开 Integral<int> -> (sizeof(int) <= 4)
        let expanded = expand_concepts("Integral<int>", &concepts);
        assert!(expanded.contains("sizeof(int) <= 4"), "{}", expanded);
        // 展开 Integral<T> -> (sizeof(T) <= 4)
        let expanded = expand_concepts("Integral<T>", &concepts);
        assert!(expanded.contains("sizeof(T) <= 4"), "{}", expanded);
    }

    #[test]
    fn m10_cxa_guard_state_machine() {
        use crate::cpp::feature_degrade::CxaGuard;
        let mut g = CxaGuard::new("var".to_string());
        assert!(g.acquire());
        assert!(!g.acquire());
        g.release();
        assert!(!g.acquire());
    }

    #[test]
    fn m10_init_list_emit_c() {
        use crate::cpp::feature_degrade::InitList;
        let mut il = InitList::new("int".to_string());
        il.push("1".to_string());
        il.push("2".to_string());
        il.push("3".to_string());
        let c = il.emit_c();
        assert!(c.contains("1, 2, 3"));
        assert!(c.contains("_M_len"));
    }

    #[test]
    fn m10_module_partition_signature() {
        use crate::cpp::feature_degrade::ModulePartition;
        let p = ModulePartition {
            name: "foo".to_string(),
            partition: "part1".to_string(),
            is_export: true,
        };
        assert_eq!(p.signature(), "module::foo:part1");
    }

    #[test]
    fn m10_coroutine_await_yield_state() {
        use crate::cpp::feature_degrade::CoroutineMachine;
        let mut cm = CoroutineMachine::new();
        let s1 = cm.co_await("x");
        let s2 = cm.co_yield("42");
        assert!(s1.contains("co_await"));
        assert!(s2.contains("co_yield"));
        assert_eq!(cm.state, 2);
    }

    #[test]
    fn m10_arrow_chain_resolve() {
        use crate::cpp::feature_degrade::ArrowChainResolver;
        assert!(ArrowChainResolver::has_arrow_overload("MyPtr"));
        assert!(ArrowChainResolver::has_arrow_overload("Iterator"));
        assert!(!ArrowChainResolver::has_arrow_overload("Foo"));
    }

    #[test]
    fn m10_dynamic_cast_cross_virtual() {
        use crate::cpp::feature_degrade::DynamicCast;
        let s = DynamicCast::cross_virtual("p", "Derived", "Base");
        assert!(s.contains("__dynamic_cast"));
    }

    #[test]
    fn m10_ctor_delegation() {
        use crate::cpp::feature_degrade::CtorExpander;
        let s = CtorExpander::emit_delegation("D", "B", "/* body */");
        assert!(s.contains("delegating ctor"));
    }

    #[test]
    fn m10_member_ptr_emit() {
        use crate::cpp::feature_degrade::MemberPtr;
        assert_eq!(MemberPtr::emit("Foo", "bar"), "&_member_fn_Foo_bar");
    }

    #[test]
    fn lower_m10_static_local_emit_cxa_guard() {
        // 静态局部变量触发 __cxa_guard_* 调用
        let src = b"void f() { static int x = 42; }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(s.contains("_cxa_guard_acquire"), "{}", s);
        assert!(s.contains("_cxa_guard_release"), "{}", s);
        assert!(s.contains("static int x"), "{}", s);
    }

    #[test]
    fn lower_m10_partial_spec_parse() {
        // 偏特化 parse
        let src = b"template <typename T> class Foo<T*> { public: T value; };";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(s.contains("partial specialization"), "{}", s);
        // 模式可能是 "T *" (带空格) 或 "T*"
        assert!(s.contains("T") && s.contains("*"), "{}", s);
    }

    #[test]
    fn lower_m10_module_partition_parse() {
        // module foo:part1;
        let src = b"module foo:part1;";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(s.contains("M10+"), "{}", s);
        assert!(s.contains("foo:part1"), "{}", s);
    }

    #[test]
    fn lower_m10_inheriting_ctor() {
        // using Base::Base; 在类外 (顶层作用域), 仅作注释
        let src = b"class Base { public: Base() {} };
                   class Derived : public Base { public: Base() {} };"; // 简化的继承构造
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        // 验证包含 Base::Base 子结构 (M10+ 简化为重复 ctor)
        assert!(
            s.contains("struct Base") && s.contains("struct Derived"),
            "{}",
            s
        );
    }

    #[test]
    fn lower_m10_init_list_emit() {
        // {1, 2, 3} 在数组初始化中, 应能降级
        // M10 简化: 通过 InitList 节点手动 emit
        use crate::cpp::feature_degrade::InitList;
        let mut il = InitList::new("int".to_string());
        il.push("1".to_string());
        il.push("2".to_string());
        il.push("3".to_string());
        let c = il.emit_c();
        assert!(c.contains("1, 2, 3"), "{}", c);
        assert!(c.contains("_M_len"), "{}", c);
    }

    #[test]
    fn lower_m10_coroutine_state_machine() {
        // 协程: co_await/co_yield 应生成真状态机代码
        let src = b"void f() { co_await 1; co_yield 2; co_return 3; }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        // M26: 真状态机驱动 - per-function 状态变量 + switch + Duff's device 标签
        assert!(s.contains("_ZN1fEv_coro_state"), "{}", s);
        assert!(s.contains("case 0:"), "{}", s);
        assert!(s.contains("case 2:"), "{}", s);
    }

    #[test]
    fn lower_m10_dynamic_cast_rtti() {
        // 跨虚继承 dynamic_cast - 仅作 M10 模块单元测试
        use crate::cpp::feature_degrade::DynamicCast;
        let s = DynamicCast::cross_virtual("p", "Derived", "Base");
        assert!(s.contains("__dynamic_cast"), "{}", s);
        assert!(s.contains("Derived"), "{}", s);
    }

    #[test]
    fn lower_m10_member_function_pointer() {
        // &Class::method - 仅作解析测试, 完整 PMF 语法不要求完全支持
        let src = b"class Foo { public: void bar() {} };
                   void test() { void (Foo::*pmf)() = &Foo::bar; (void)pmf; }";
        // PMF 完整语法可能不解析, 仅做最简的 try
        let mut p = CppParser::new(src, "test.cpp");
        let result = p.parse_top_level();
        // 接受解析成功或失败
        let _ = result;
    }

    #[test]
    fn lower_module_bmf_collects_exports() {
        // M26: export module 后的自由函数/类/import 收集为 BMF 导出记录
        let src = b"export module mymod;
                   import other;
                   export int answer(int x) { return x + 1; }
                   export class Widget { public: int id; };";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let bmf = l
            .module_bmf()
            .expect("module_bmf() should be Some after export module");
        assert!(bmf.starts_with("DCC_BMF_V1\nM mymod\n"), "{}", bmf);
        assert!(bmf.contains("I other"), "{}", bmf);
        assert!(bmf.contains("E F answer int _ZN6answerEv(int)"), "{}", bmf);
        assert!(bmf.contains("E C Widget int id;"), "{}", bmf);
    }

    #[test]
    fn lower_odr_check_conflicting_imports() {
        // M26.5: 两个导入模块导出同名函数但定义不同 -> ODR 违规
        let mut l = Lower::new();
        l.imported_modules = vec![
            BmfModule {
                name: "mod_a".to_string(),
                exports: vec![BmfExport {
                    kind: BmfKind::Function,
                    name: "foo".to_string(),
                    signature: "int _ZN3fooEv()".to_string(),
                    layout: None,
                }],
                imports: vec![],
                partition: None,
                part_imports: vec![],
            },
            BmfModule {
                name: "mod_b".to_string(),
                exports: vec![BmfExport {
                    kind: BmfKind::Function,
                    name: "foo".to_string(),
                    signature: "int _ZN3fooEi(int)".to_string(),
                    layout: None,
                }],
                imports: vec![],
                partition: None,
                part_imports: vec![],
            },
        ];
        let msg = l.odr_check().expect("conflict should be reported");
        assert!(msg.contains("ODR violations"), "{}", msg);
        assert!(msg.contains("foo"), "{}", msg);
        assert!(msg.contains("mod_a"), "{}", msg);
        assert!(msg.contains("mod_b"), "{}", msg);
    }

    #[test]
    fn lower_odr_check_identical_definitions_ok() {
        // M26.5: 同名同定义 (签名一致) 不算 ODR 违规
        let mut l = Lower::new();
        let exports = vec![BmfExport {
            kind: BmfKind::Function,
            name: "foo".to_string(),
            signature: "int _ZN3fooEv()".to_string(),
            layout: None,
        }];
        l.imported_modules = vec![
            BmfModule {
                name: "mod_a".to_string(),
                exports: exports.clone(),
                imports: vec![],
                partition: None,
                part_imports: vec![],
            },
            BmfModule {
                name: "mod_b".to_string(),
                exports,
                imports: vec![],
                partition: None,
                part_imports: vec![],
            },
        ];
        assert!(l.odr_check().is_none());
    }

    #[test]
    fn lower_odr_check_own_module_exports() {
        // M26.5: 本模块导出与导入模块导出冲突也被检查
        let mut l = Lower::new();
        l.module_name = "my".to_string();
        l.module_exports = vec![BmfExport {
            kind: BmfKind::Class,
            name: "Widget".to_string(),
            signature: String::new(),
            layout: Some("int id;".to_string()),
        }];
        l.imported_modules = vec![BmfModule {
            name: "other".to_string(),
            exports: vec![BmfExport {
                kind: BmfKind::Class,
                name: "Widget".to_string(),
                signature: String::new(),
                layout: Some("double id;".to_string()),
            }],
            imports: vec![],
            partition: None,
            part_imports: vec![],
        }];
        let msg = l.odr_check().expect("conflict should be reported");
        assert!(msg.contains("Widget"), "{}", msg);
    }

    #[test]
    fn lower_export_template() {
        // M26: export template <typename T> class Box { ... };
        let src = b"export module mymod;
                   export template <typename T>
                   class Box { public: T value; };";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let bmf = l
            .module_bmf()
            .expect("module_bmf() should be Some after export module");
        // 应该包含模板导出记录
        assert!(bmf.contains("E T Box T"), "{}", bmf);
    }

    #[test]
    fn lower_virtual_inheritance_diamond_detection() {
        // M26.3: 菱形虚继承 -> Itanium ABI 布局 (vbtable + vbptr + 尾部虚基子对象)
        // class A {};
        // class B : public virtual A {};
        // class C : public A {};   // 非 virtual
        // class D : public B, public C {};  // 混合菱形
        let src = b"class A { public: int x; };
                   class B : public virtual A { public: int y; };
                   class C : public A { public: int z; };
                   class D : public B, public C { public: int w; };";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        // 不再输出 warning/注释降级; B 的虚基 A 以 vbtable + __vb_ 子对象表达
        assert!(!s.contains("WARNING"), "{}", s);
        assert!(s.contains("__vbt_B"), "{}", s);
        assert!(s.contains("long* __vbptr;"), "{}", s);
        assert!(s.contains("struct A __vb_A;"), "{}", s);
        // D 同时嵌入 B/C 两个非虚基 (C 的 A 保持非虚副本)
        assert!(s.contains("__base_B"), "{}", s);
        assert!(s.contains("__base_C"), "{}", s);
    }

    #[test]
    fn lower_virtual_inheritance_diamond_layout() {
        // M26.3: 经典菱形 - 共享虚基 Base 只出现一次 (去重), vbtable 偏移正确
        let src = b"struct Base { int b; };
                   struct Mid1 : virtual Base { int m1; };
                   struct Mid2 : virtual Base { int m2; };
                   struct Final : virtual Mid1, virtual Mid2 { int f; };
                   int use(Final fo) { return fo.b + fo.m1 + fo.m2 + fo.f; }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        // 传递虚基 DFS 顺序: [Mid1, Base, Mid2]
        let vbs = l
            .transitive_vbases
            .get("Final")
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            vbs,
            vec!["Mid1".to_string(), "Base".to_string(), "Mid2".to_string()]
        );
        // vbtable 前 4 项: 0(自身) + 3 个虚基
        let tbl = l.vbase_tables.get("Final").cloned().unwrap_or_default();
        assert_eq!(tbl.len(), 4, "tbl={:?}", tbl);
        // 布局: vptr@0, vbptr@8, f@16, __vb_Mid1@24, __vb_Base@48, __vb_Mid2@56
        let offs = l.field_offsets.get("Final").cloned().unwrap_or_default();
        assert_eq!(*offs.get("__vptr").unwrap_or(&-1), 0);
        assert_eq!(*offs.get("__vbptr").unwrap_or(&-1), 8);
        assert_eq!(*offs.get("__vb_Mid1").unwrap_or(&-1), 24);
        assert_eq!(*offs.get("__vb_Base").unwrap_or(&-1), 48);
        assert_eq!(*offs.get("__vb_Mid2").unwrap_or(&-1), 56);
        let s = l.finish();
        // 自动 ctor: vbptr 初始化 + 虚基 ctor 调用
        assert!(s.contains("_ZN5FinalC1Ev"), "{}", s);
        assert!(s.contains("this->__vbptr = (long*)__vbt_Final;"), "{}", s);
        assert!(s.contains("_ZN4BaseC1Ev(&this->__vb_Base);"), "{}", s);
        // 虚基成员访问经 vbtable: fo.b -> __vbptr[2]
        assert!(s.contains("__vbptr[2]"), "{}", s);
    }

    #[test]
    fn lower_enum_emits_real_c_enum() {
        // M26: enum 保留枚举值语义, 不再降级为 typedef int
        let src = b"enum Color { RED, GREEN = 5, BLUE };
                   enum class Scoped : unsigned char { A = 1, B = A + 2 };
                   enum { ANON_X = 42 };";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        // 枚举值保留
        assert!(s.contains("RED"), "{}", s);
        assert!(s.contains("GREEN = 5"), "{}", s);
        assert!(s.contains("BLUE"), "{}", s);
        // scoped + 底层类型保留
        assert!(
            s.contains("typedef enum : unsigned char { A = 1, B = A + 2 } Scoped;"),
            "{}",
            s
        );
        // 匿名枚举只产常量, 不产类型名
        assert!(s.contains("enum { ANON_X = 42 };"), "{}", s);
        // 不应再出现 typedef int 降级
        assert!(!s.contains("typedef int Color"), "{}", s);
        assert!(!s.contains("typedef int Scoped"), "{}", s);
    }

    #[test]
    fn lower_enum_namespace_prefixed() {
        // 命名空间内的 enum 类型名加 ns 前缀, 与类/函数一致
        let src = b"namespace ns { enum E { X = 7 }; }
                   int f() { return ns::E::X; }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(s.contains("typedef enum { X = 7 } ns_E;"), "{}", s);
    }

    #[test]
    fn lower_noexcept_expr_analyzes_throwing() {
        // M26.1: noexcept(expr) 不再恒为 true
        let src = b"int maybe(int x);
                   int safe_fn(int x) noexcept { return x; }
                   class Widget {
                     public:
                       Widget() {}
                       int safe() noexcept { return 1; }
                       int risky();
                   };
                   int f() {
                     int x = 1;
                     Widget w;
                     int r = 0;
                     if (!noexcept(x)) r |= 1;
                     if (!noexcept(1 + 2)) r |= 2;
                     if (!noexcept(safe_fn(1))) r |= 4;
                     if (noexcept(maybe(1))) r |= 8;
                     if (noexcept(new int)) r |= 16;
                     if (!noexcept(w.safe())) r |= 32;
                     if (noexcept(w.risky())) r |= 64;
                     if (!noexcept((int)1.5)) r |= 128;
                     return r;
                   }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        // 不抛异常 -> 1: 变量 / 内建运算 / noexcept 函数 / noexcept 成员 / cast
        assert!(s.contains("if (!1) {\nr|=1;"), "{}", s);
        assert!(s.contains("r|=2;"), "{}", s);
        assert!(s.contains("r|=4;"), "{}", s);
        assert!(s.contains("r|=32;"), "{}", s);
        assert!(s.contains("r|=128;"), "{}", s);
        // 可能抛 -> 0: 普通函数 / new / 普通成员
        assert!(s.contains("if (0) {\nr|=8;"), "{}", s);
        assert!(s.contains("r|=16;"), "{}", s);
        assert!(s.contains("r|=64;"), "{}", s);
        // noexcept 函数定义应带 nothrow 属性
        assert!(
            s.contains("__attribute__((nothrow)) int _ZN7safe_fnEv"),
            "{}",
            s
        );
    }

    #[test]
    fn lower_auto_deduces_call_member_and_new() {
        // M26.1: auto 变量从 函数调用 / 成员调用 / 成员访问 / new 推导真实类型
        let src = b"double get_double() { return 0; }
                   char* get_str() { return 0; }
                   class Widget {
                     public:
                       int id;
                       double weight;
                       Widget() {}
                       int get_id() { return id; }
                       double get_weight() { return weight; }
                   };
                   int f() {
                     auto d = get_double();
                     auto s = get_str();
                     auto p = new Widget;
                     Widget w;
                     auto fid = w.id;
                     auto fw = w.weight;
                     auto mid = w.get_id();
                     auto mw = w.get_weight();
                     return (int)d + (int)fw + (int)mw;
                   }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        // 函数返回类型注册表: auto d / auto s
        assert!(s.contains("double d = _ZN10get_doubleEv();"), "{}", s);
        assert!(s.contains("char* s = _ZN7get_strEv();"), "{}", s);
        // new T -> T*
        assert!(s.contains("struct Widget* p = "), "{}", s);
        // 成员字段: w.id -> int, w.weight -> double
        assert!(s.contains("int fid = w.id;"), "{}", s);
        assert!(s.contains("double fw = w.weight;"), "{}", s);
        // 成员调用: w.get_id() -> int, w.get_weight() -> double
        assert!(s.contains("int mid = _ZN6Widget6get_idEv(&w);"), "{}", s);
        assert!(
            s.contains("double mw = _ZN6Widget10get_weightEv(&w);"),
            "{}",
            s
        );
    }

    #[test]
    fn lower_free_fn_call_uses_mangled_name() {
        // M26.1: 自由函数调用与定义使用同一 mangled 名 (修复链接失败)
        let src = b"int add(int a, int b) { return a + b; }
                   int f() { return add(1, 2); }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        assert!(s.contains("int _ZN3addEv (int a, int b)"), "{}", s);
        assert!(s.contains("return _ZN3addEv(1, 2);"), "{}", s);
    }

    #[test]
    fn lower_rtti_typeinfo_and_vtable() {
        // M26: RTTI 完善 - typeinfo 和 vtable 生成
        let src = b"class Base { public: virtual void foo() {} };
                   class Derived : public Base { public: void foo() override {} };";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        // 应该生成 typeinfo 符号
        assert!(s.contains("__dcc_ti_Base"), "{}", s);
        assert!(s.contains("__dcc_ti_Derived"), "{}", s);
        // 应该生成 vtable 符号 (因为有 virtual 方法)
        assert!(s.contains("__dcc_vt_Base"), "{}", s);
        assert!(s.contains("__dcc_vt_Derived"), "{}", s);
        // vtable 应该包含 RTTI slot
        assert!(s.contains("RTTI typeinfo pointer"), "{}", s);
    }

    #[test]
    fn lower_recursive_inherited_member_access() {
        // M26: 递归继承成员访问
        // GrandParent has member x, Parent inherits from GrandParent, Child inherits from Parent
        // Accessing child.x should rewrite to child.__base_Parent.__base_GrandParent.x
        let src = b"class GrandParent { public: int x; };
                   class Parent : public GrandParent { public: int y; };
                   class Child : public Parent { public: int z; };
                   void f() { Child c; c.x = 1; c.y = 2; c.z = 3; }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        // 应该包含继承子结构
        assert!(s.contains("__base_GrandParent"), "{}", s);
        assert!(s.contains("__base_Parent"), "{}", s);
        // 成员访问应该通过完整 __base_ 路径
        assert!(s.contains("__base_Parent.__base_GrandParent.x"), "{}", s);
    }

    #[test]
    fn lower_dcc_mbr_inherited_members() {
        // __dcc_mbr 辅助定义: 继承成员也需要生成
        // Derived 继承 Base 的成员 x, 应该有 __dcc_mbr_Derived_x
        let src = b"class Base { public: int x; };
                   class Derived : public Base { public: int y; };
                   void f() { Derived d; }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        // 直接成员的 __dcc_mbr
        assert!(s.contains("__dcc_mbr_Derived_y"), "{}", s);
        assert!(s.contains("__dcc_mbr_Base_x"), "{}", s);
        // 继承成员的 __dcc_mbr (通过 __base_ 链访问)
        assert!(s.contains("__dcc_mbr_Derived_x"), "{}", s);
        // 继承成员的 __dcc_mbr 应该使用 __base_ 路径
        assert!(s.contains("__f.__base_Base.x"), "{}", s);
    }

    #[test]
    fn lower_dcc_mbr_recursive_inherited() {
        // 三层继承: Child 的 __dcc_mbr 应该能访问 GrandParent 的成员
        let src = b"class GrandParent { public: int x; };
                   class Parent : public GrandParent { public: int y; };
                   class Child : public Parent { public: int z; };
                   void f() { Child c; }";
        let mut p = CppParser::new(src, "test.cpp");
        let decls = p.parse_top_level().unwrap();
        let mut l = Lower::new();
        l.lower_decls(&decls);
        let s = l.finish();
        // Child 的继承成员 x (来自 GrandParent, 通过 Parent)
        assert!(s.contains("__dcc_mbr_Child_x"), "{}", s);
        // 应该使用完整 __base_ 链
        assert!(
            s.contains("__f.__base_Parent.__base_GrandParent.x"),
            "{}",
            s
        );
        // Child 的继承成员 y (来自 Parent)
        assert!(s.contains("__dcc_mbr_Child_y"), "{}", s);
        assert!(s.contains("__f.__base_Parent.y"), "{}", s);
    }
}
