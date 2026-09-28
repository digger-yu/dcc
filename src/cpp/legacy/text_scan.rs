//! Legacy C 文本流水线的**字节级扫描原语**。
//!
//! Stage 1 拆分: 从 `lower.rs` 原样搬来, 行为不变。这些函数只做「在 C 源码文本里
//! 定位一个词 / 跳过一段字符串或注释」, 是全部文本改写 pass 的共同底座; 单独成模块
//! 的理由是它们被 `lower.rs` 与 `legacy::runtime` 两个地方共用, 留在任一处都会让
//! 另一处反向依赖具体的改写逻辑。
//!
//! 关键约定 (与历史行为一致, 也是这些函数存在的意义):
//! - 字符串/字符字面量与注释里的内容**不是**代码: 逐字节扫描必须先跳过它们,
//!   否则关键字剥离会失步 (见 `skip_string_literal` 的字符字面量陷阱注释)。

/// M18.0: 检查 ident 是否是 C 关键字 (如 int/char/void/...), 不能作为 typedef 名
pub(crate) fn is_c_keyword(s: &str) -> bool {
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
pub(crate) fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// 检查 bytes[pos..] 是否以 word 开头且前后是标识符边界
pub(crate) fn match_word_at(bytes: &[u8], pos: usize, word: &str) -> bool {
    let wlen = word.len();
    if pos + wlen > bytes.len() {
        return false;
    }
    if &bytes[pos..pos + wlen] != word.as_bytes() {
        return false;
    }
    let before_ok = pos == 0 || !is_ident_char(bytes[pos - 1]);
    let after_ok = pos + wlen >= bytes.len() || !is_ident_char(bytes[pos + wlen]);
    before_ok && after_ok
}

/// 跳过 C 风格字符串字面量 "..." (含转义)
/// 注意: 调用方逐字符扫描时, 字符字面量 `'"'` 的引号会被误认为
/// 字符串开头 (GCC 源码如 libcpp/mkdeps.cc 大量使用 `fputc ('"', fp)`),
/// 一旦误判就会吞掉到下一个 `"` 为止的全部代码, 使后续模板展平 /
/// 关键字剥离全部失步. 因此这里检测前导 `'`, 将 `"` 视为普通字符.
pub(crate) fn skip_string_literal(bytes: &[u8], mut pos: usize) -> usize {
    if pos >= bytes.len() || bytes[pos] != b'"' {
        return pos;
    }
    // 前一个字符是 `'` → 这是字符字面量 `'"'` 的引号, 不是字符串
    if pos > 0 && bytes[pos - 1] == b'\'' {
        // 但若前导 `'` 自身是 `\'` 转义的一部分 (即处于字符串内),
        // 不应到这里; 逐字符扫描的调用方只会把真正的字符串开头交进来.
        return pos + 1;
    }
    pos += 1;
    while pos < bytes.len() {
        if bytes[pos] == b'\\' {
            pos += 2;
            continue;
        }
        if bytes[pos] == b'"' {
            return pos + 1;
        }
        pos += 1;
    }
    pos
}

/// 跳过 // 或 /* */ 注释
pub(crate) fn skip_comment(bytes: &[u8], mut pos: usize) -> usize {
    if pos + 1 >= bytes.len() {
        return pos;
    }
    if bytes[pos] == b'/' && bytes[pos + 1] == b'/' {
        while pos < bytes.len() && bytes[pos] != b'\n' {
            pos += 1;
        }
        return pos;
    }
    if bytes[pos] == b'/' && bytes[pos + 1] == b'*' {
        pos += 2;
        while pos + 1 < bytes.len() {
            if bytes[pos] == b'*' && bytes[pos + 1] == b'/' {
                return pos + 2;
            }
            pos += 1;
        }
        return bytes.len();
    }
    pos
}

/// 读取一个标识符, 返回 (ident_text, end_pos)
pub(crate) fn read_ident(bytes: &[u8], mut pos: usize) -> (String, usize) {
    let start = pos;
    while pos < bytes.len() && is_ident_char(bytes[pos]) {
        pos += 1;
    }
    (String::from_utf8_lossy(&bytes[start..pos]).to_string(), pos)
}

/// 跳过空白 (空格/Tab/换行)
pub(crate) fn skip_ws(bytes: &[u8], mut pos: usize) -> usize {
    while pos < bytes.len() && (bytes[pos] as char).is_ascii_whitespace() {
        pos += 1;
    }
    pos
}
