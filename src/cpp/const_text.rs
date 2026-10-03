//! 文本形式整数常量表达式求值 ( dcc++ native 语义层的"字面量单一真源")
//!
//! ## 为什么需要这个模块
//!
//! parser 对 `enum X { A = 1, B = A + 2 };` 这类声明只保留 **初始化器源码
//! 文本** (`CppDecl::Enum::variants: Vec<(String, Option<String>)>`), 拿不到
//! 结构化的 `CppExpr`。历史上字面量 parse 至少散落三处
//! (`parser::parse_int_text`、`template::parse_int_text`、`sema` 里两处内联
//! 复制), 且全都只认纯字面量 (十进制/十六进制), 于是:
//!
//! * `B = A + 2` 求值失败 → 枚举常量值被静默丢弃;
//! * `1 << 3` / `'A'` / `~0` 等常见写法同样丢失。
//!
//! 本模块集中提供唯一一份"文本 → i128 常量"的求值实现: 先 tokenize, 再按
//! C++ 运算符优先级做递归下降。任何需要折叠整数常量文本的语义层调用点都应
//! 走这里, 不要再复制 parse。
//!
//! [`crate::cpp::sema`] 用它在登记枚举常量时记录真实值。

use std::collections::HashMap;

/// 解析单个整数字面量文本。
///
/// 支持十进制、`0x` 十六进制、`0b` 二进制、`0` 八进制, C++14 的 `'`
/// 数字分隔符, 以及 u/U/l/L 整数后缀。失败返回 `None`。
pub fn parse_integer_literal_text(text: &str) -> Option<i128> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    // 只允许 数字 / a-f A-F / x X / '_' / 整数后缀 字符, 其它一律不是字面量
    if !t
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '\'')
    {
        return None;
    }
    // 剥掉整数后缀 (u/U/l/L 的任意组合)
    let digits = t.trim_end_matches(['u', 'U', 'l', 'L']);
    if digits.is_empty() {
        return None;
    }
    let cleaned: String = digits.chars().filter(|&c| c != '\'').collect();
    if cleaned.is_empty() {
        return None;
    }
    let (radix, rest) = if let Some(h) = cleaned
        .strip_prefix("0x")
        .or_else(|| cleaned.strip_prefix("0X"))
    {
        (16, h.to_string())
    } else if let Some(b) = cleaned
        .strip_prefix("0b")
        .or_else(|| cleaned.strip_prefix("0B"))
    {
        (2, b.to_string())
    } else if cleaned.len() > 1 && cleaned.starts_with('0') {
        (8, cleaned[1..].to_string())
    } else {
        (10, cleaned.clone())
    };
    if rest.is_empty() {
        return None;
    }
    if !rest.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    i128::from_str_radix(&rest, radix).ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ConstTok<'a> {
    Num(i128),
    Ident(&'a str),
    Punct(&'a str),
}

const LONG_PUNCT: &[&str] = &["<<", ">>", "<=", ">=", "==", "!=", "&&", "||"];
const SHORT_PUNCT: &[&str] = &[
    "+", "-", "*", "/", "%", "&", "|", "^", "~", "!", "(", ")", "<", ">", "?", ":", ",",
];

/// tokenize 常量表达式文本; 遇到无法识别的字符 (如 `.` / `::`) 返回 `None`。
fn tokenize_const_text(text: &str) -> Option<Vec<ConstTok<'_>>> {
    let mut out: Vec<ConstTok> = Vec::new();
    let mut rest = text;
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        let b = rest.as_bytes();
        let c = b[0] as char;
        if c == '\'' {
            // 字符字面量: 'A' / '\n' / '\\' / '\0'
            let (val, consumed) = parse_char_literal(rest)?;
            out.push(ConstTok::Num(val));
            rest = &rest[consumed..];
            continue;
        }
        if c.is_ascii_digit() {
            let end = rest
                .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '\''))
                .unwrap_or(rest.len());
            let lit = &rest[..end];
            out.push(ConstTok::Num(parse_integer_literal_text(lit)?));
            rest = &rest[end..];
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let end = rest
                .find(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
                .unwrap_or(rest.len());
            out.push(ConstTok::Ident(&rest[..end]));
            rest = &rest[end..];
            continue;
        }
        if let Some(lp) = LONG_PUNCT.iter().find(|p| rest.starts_with(**p)) {
            out.push(ConstTok::Punct(lp));
            rest = &rest[lp.len()..];
            continue;
        }
        if let Some(sp) = SHORT_PUNCT.iter().find(|p| rest.starts_with(**p)) {
            out.push(ConstTok::Punct(sp));
            rest = &rest[sp.len()..];
            continue;
        }
        return None;
    }
    Some(out)
}

/// 解析以 `'` 开头的字符字面量, 返回 (值, 消耗的字节数)。
fn parse_char_literal(text: &str) -> Option<(i128, usize)> {
    let bytes = text.as_bytes();
    if bytes.first() != Some(&b'\'') {
        return None;
    }
    let first = *bytes.get(1)?;
    let (val, close_at) = if first == b'\\' {
        // 转义序列: 与字符串字面量共用同一个解码器 (单一真源, 见
        // `decode_escape_at`), 所以 `'\101'` / `'\x41'` 与 `'A'` 同值。
        let (v, used) = decode_escape_at(bytes, 1)?;
        (i128::from(v), 1 + used)
    } else {
        (i128::from(first), 2)
    };
    if bytes.get(close_at) != Some(&b'\'') {
        return None;
    }
    Some((val, close_at + 1))
}

/// 简单转义序列 (`\n` / `\t` / ...) 的**唯一真源**表。八进制与十六进制
/// 数字转义由 `decode_escape_at` 的专用分支处理。
fn simple_escape(c: u8) -> Option<u8> {
    Some(match c {
        b'n' => b'\n',
        b't' => b'\t',
        b'r' => b'\r',
        b'a' => 7,
        b'b' => 8,
        b'f' => 12,
        b'v' => 11,
        b'\\' => b'\\',
        b'\'' => b'\'',
        b'"' => b'"',
        b'?' => b'?',
        b'e' => 27,
        _ => return None,
    })
}

/// 解码 `bytes[at] == b'\\'` 处的一个转义序列, 返回 (字节值, 消耗的字节数
/// —— 含开头的反斜杠)。不识别的转义返回 `None`。
///
/// 覆盖 C++ [lex.escape]: 简单转义、`\NNN` 八进制 (最多 3 位, 按 u8 截断)、
/// `\xHH..` 十六进制 (吸收全部连续十六进制位, 按 u8 截断, 同 GCC)。
pub(crate) fn decode_escape_at(bytes: &[u8], at: usize) -> Option<(u8, usize)> {
    if bytes.get(at) != Some(&b'\\') {
        return None;
    }
    let c = *bytes.get(at + 1)?;
    if (b'0'..=b'7').contains(&c) {
        let mut val: u32 = 0;
        let mut n = 0usize;
        while n < 3 {
            match bytes.get(at + 1 + n) {
                Some(d @ b'0'..=b'7') => {
                    val = val * 8 + u32::from(d - b'0');
                    n += 1;
                },
                _ => break,
            }
        }
        return Some((val as u8, n + 1));
    }
    if c == b'x' {
        let mut val: u32 = 0;
        let mut n = 0usize;
        while let Some(d) = bytes.get(at + 2 + n) {
            match char::from(*d).to_digit(16) {
                Some(v) => {
                    val = val.wrapping_mul(16).wrapping_add(v);
                    n += 1;
                },
                None => break,
            }
        }
        if n == 0 {
            return None;
        }
        return Some((val as u8, n + 2));
    }
    Some((simple_escape(c)?, 2))
}

/// 整数常量的类型秩 (位数): 能表示该值的**最小**有符号整数秩。
///
/// 这就是 [lex.icon] 对无后缀十进制字面量的量级规则 (int → long → long long
/// 里首个能表示者), 抽成单一真源是因为同一件事有两处需要它:
///
/// * `src/cpp/sema.rs` 给整数字面量推 HIR 类型 (以前硬写 `Integer{bits:32}`);
/// * `src/ssa/expr_rebuild.rs` 的 SSA→AST 重建 —— `Operand::Int(i128)` 本身无类型, 常量节点
///   的 `ty` 决定立即数物化宽度与 `usual_arith_type` 的推导结果。
///
/// 局限 (已记录): 本函数只看量级。字面量的**后缀** (`1L << 40` 这类小量级 +
/// 宽后缀) 与基数 (十六进制 > INT_MAX 在标准里是 `unsigned int`) 在
/// `CppExpr::IntLit(i64)` 里已被 parser 剥掉, 需要 AST 形状改动才能补齐。
pub fn int_literal_rank_bits(v: i128) -> u16 {
    if v >= i32::MIN as i128 && v <= i32::MAX as i128 {
        32
    } else {
        64
    }
}

/// 整数字面量文本的 [lex.icon] 类型: `(位数, 是否有符号, 是否 long long 档)`。
///
/// 类型由**后缀与数值量级共同**决定: 标准给每个后缀一类候选类型, 取第一个
/// 能表示该值的。十进制无后缀的候选只有有符号类型 (int → long → long long),
/// 而十六进制/八进制/二进制还穿插 unsigned —— 这正是 `0xFFFFFFFF`
/// (`unsigned int`, 乘法在 32 位绕回) 与 `4294967295` (`long`, 不绕回) 的差别。
/// `1L << 40` 同理: 量级规则只会看到 `1` → `int`, 只有后缀能把秩抬到 64 位。
///
/// 第三项区分 `long`/`long long` (本仓两者都是 64 位, 但 Itanium mangling 与
/// 模板非类型实参的符号名不同: `Tpl<42LL>` 里的字面量类型参与 mangle)。
///
/// 无法解析的文本返回 `None` (调用方保持原有 `int` 处理, 不猜类型)。
pub fn int_literal_type(text: &str) -> Option<(u16, bool, bool)> {
    let (bits, signed) = int_literal_candidate_rank(text)?;
    // `long long` 档只可能由后缀强制 (`ll`/`ull`); 无后缀时标准先选 `long`。
    let ll = split_int_lit_suffix(text.trim())
        .1
        .to_ascii_lowercase()
        .matches('l')
        .count()
        >= 2;
    Some((bits, signed, ll))
}

/// [`int_literal_type`] 的前半段: 候选表 + 量级筛选, 返回 `(位数, 符号性)`。
/// 单独的秩查询 (不关心 long long 档) 走这里。
fn int_literal_candidate_rank(text: &str) -> Option<(u16, bool)> {
    let v = parse_integer_literal_text(text)?;
    let t = text.trim();
    // 后缀 = 尾部连续的 u/U/l/L 字符; 剩下的数码部分用于判进位制
    let (digits, suffix_raw) = split_int_lit_suffix(t);
    let suffix = suffix_raw.to_ascii_lowercase();
    let has_u = suffix.contains('u');
    let l_rank = suffix.matches('l').count().min(2);
    let plain_dec = {
        let d = digits.replace('\'', "");
        !(d.starts_with("0x")
            || d.starts_with("0X")
            || d.starts_with("0b")
            || d.starts_with("0B")
            || d.starts_with("0o")
            || d.starts_with("0O")
            || (d.len() > 1 && d.starts_with('0')))
    };

    // 候选秩: (位数, 符号性), 按 [lex.icon]p5 的表顺序
    let cands: &[(u16, bool)] = if has_u {
        if l_rank == 0 {
            &[(32, false), (64, false)]
        } else {
            &[(64, false)]
        }
    } else if l_rank >= 1 {
        &[(64, true)]
    } else if plain_dec {
        &[(32, true), (64, true)]
    } else {
        &[(32, true), (32, false), (64, true), (64, false)]
    };
    cands
        .iter()
        .copied()
        .find(|&(bits, signed)| {
            if signed {
                let half: i128 = 1i128 << (bits - 1);
                v >= -half && v < half
            } else {
                v >= 0 && v < (1i128 << bits)
            }
        })
        .or_else(|| cands.last().copied())
}

/// 整数字面量文本的量级秩 (32/64): 与 [`int_literal_type`] 共用同一份
/// 候选表, 但只看数值 —— 供只拿得到**值** (后缀已在词法边界剥掉) 的
/// 调用点使用: [`crate::cpp::sema`] 的 `IntLit` 类型标注, 以及
/// [`crate::ssa`] SSA 常量物化的 `int_const_ty`。
pub fn int_literal_rank_only(text: &str) -> Option<u16> {
    int_literal_candidate_rank(text).map(|(bits, _)| bits)
}

/// 剥开整数字面量的后缀: 返回 (数码部分, 后缀部分)。
pub fn split_int_lit_suffix(text: &str) -> (&str, &str) {
    let bytes = text.as_bytes();
    let mut i = bytes.len();
    while i > 0 && matches!(bytes[i - 1], b'u' | b'U' | b'l' | b'L') {
        i -= 1;
    }
    // 全字都是后缀 (如孤立 `u`) 不当成字面量
    if i == 0 {
        return (text, "");
    }
    (&text[..i], &text[i..])
}

/// 剥开浮点字面量的后缀: 返回 (数值部分, 后缀部分)。
///
/// [lex.fcon]: 后缀只表**精度** —— `f`/`F` 是 `float`, `l`/`L` 是
/// `long double`, 无后缀是 `double`。十六进制浮点 (`0x1p3`) 的后缀同样在末尾。
/// 全字都是后缀时不当成字面量 (返回原文与空后缀)。
fn split_float_lit_suffix(text: &str) -> (&str, &str) {
    let bytes = text.as_bytes();
    let mut i = bytes.len();
    while i > 0 && matches!(bytes[i - 1], b'f' | b'F' | b'l' | b'L') {
        i -= 1;
    }
    if i == 0 {
        return (text, "");
    }
    (&text[..i], &text[i..])
}

/// 浮点字面量的类型宽度 (32 / 64 / 80 位), 单一真源。
///
/// 早前 C++ 前端把所有浮点常量按 double 建模并在语义入口剥掉后缀, 于是
/// `long double a = 1.0L / 3.0L` 的两个操作数都是 double, 结果只有 53 位精度
/// (实测 dcc `0.333333333333333315` / g++ `0.333333333333333333`), 且
/// `float` 字面量也一样失去 `float` 类型。后缀携带的是**类型**信息, 必须在
/// 语义层用掉而不是丢弃。
pub fn float_literal_bits(text: &str) -> u16 {
    let suffix = split_float_lit_suffix(text).1;
    // `f` = float, `l` = long double; 空后缀 = double。同时出现两个后缀
    // (`1.0fl`) 不是合法字面量, 按 double 处理即可。
    match suffix {
        "f" | "F" => 32,
        "l" | "L" => 80,
        _ => 64,
    }
}

/// 剥掉浮点字面量后缀, 返回可被 `f64::from_str` / [`crate::float80::from_decimal`]
/// 接受的数值部分。
pub fn float_literal_digits(text: &str) -> String {
    split_float_lit_suffix(text).0.to_string()
}

/// 解码字符串字面量**内容** (已去引号) 为真实字节序列。
///
/// 未转义部分按原字节保留 (包括 UTF-8 多字节序列); 无法识别的转义丢弃
/// 反斜杠并保留后一个字符 (`\d` → `d`, 与 GCC 对非标准转义的处理一致)。
/// 末尾**不**追 `\0` —— [lex.string] 要求的终止符由调用方 (`lower_ir`) 补,
/// 以保持 `char[N]` 尺寸/`sizeof` 的算账在一处。
pub fn decode_string_escapes(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        match decode_escape_at(bytes, i) {
            Some((v, used)) => {
                out.push(v);
                i += used;
            },
            None => {
                i += 1;
                if i < bytes.len() {
                    out.push(bytes[i]);
                    i += 1;
                }
            },
        }
    }
    out
}

/// 文本形式整数常量表达式求值。
///
/// `env` 提供可引用的具名常量 (如已登记的前序枚举常量)。
/// 支持: 整数/字符字面量、具名常量、一元 `+ - ~ !`、括号, 以及 C++ 完整
/// 二元整数运算符优先级 (`|| && | ^ & == != < > << >> + - * / %`)。
/// 无法求值 (未知标识符、函数调用、`sizeof`、三元等) 返回 `None`。
pub fn eval_integer_const_text(text: &str, env: &HashMap<String, i128>) -> Option<i128> {
    let toks = tokenize_const_text(text)?;
    if toks.is_empty() {
        return None;
    }
    let mut p = ConstTextParser { toks, pos: 0, env };
    let v = p.parse_binary(0)?;
    if p.pos != p.toks.len() {
        // 有尾随垃圾 (如 `A B`, `1 2`) → 不是合法常量表达式
        return None;
    }
    Some(v)
}

struct ConstTextParser<'a> {
    toks: Vec<ConstTok<'a>>,
    pos: usize,
    env: &'a HashMap<String, i128>,
}

impl<'a> ConstTextParser<'a> {
    fn peek_tok(&self) -> Option<ConstTok<'a>> {
        self.toks.get(self.pos).cloned()
    }

    fn eat_punct(&mut self, p: &str) -> bool {
        match self.peek_tok() {
            Some(ConstTok::Punct(s)) if s == p => {
                self.pos += 1;
                true
            },
            _ => false,
        }
    }

    /// 二元运算符优先级 (值越小越松)。非二元运算符返回 `None`。
    fn prec(p: &str) -> Option<u8> {
        Some(match p {
            "||" => 0,
            "&&" => 1,
            "|" => 2,
            "^" => 3,
            "&" => 4,
            "==" | "!=" => 5,
            "<" | ">" | "<=" | ">=" => 6,
            "<<" | ">>" => 7,
            "+" | "-" => 8,
            "*" | "/" | "%" => 9,
            _ => return None,
        })
    }

    fn parse_binary(&mut self, min_prec: u8) -> Option<i128> {
        let mut lhs = self.parse_unary()?;
        // 只有标点 token 才可能是二元运算符: peek 到非标点 (或末尾) 即结束
        while let Some(ConstTok::Punct(op)) = self.peek_tok() {
            let prec = match Self::prec(op) {
                Some(p) => p,
                None => break,
            };
            if prec < min_prec {
                break;
            }
            self.pos += 1;
            // 左结合: 右侧用更高优先级
            let rhs = self.parse_binary(prec + 1)?;
            lhs = self.apply_binop(op, lhs, rhs)?;
        }
        Some(lhs)
    }

    fn apply_binop(&self, op: &str, l: i128, r: i128) -> Option<i128> {
        Some(match op {
            "+" => l.wrapping_add(r),
            "-" => l.wrapping_sub(r),
            "*" => l.wrapping_mul(r),
            "/" => {
                if r == 0 {
                    return None;
                }
                l.wrapping_div(r)
            },
            "%" => {
                if r == 0 {
                    return None;
                }
                l.wrapping_rem(r)
            },
            "&" => l & r,
            "|" => l | r,
            "^" => l ^ r,
            "<<" => {
                if !(0..128).contains(&r) {
                    return None;
                }
                l.wrapping_shl(r as u32)
            },
            ">>" => {
                if !(0..128).contains(&r) {
                    return None;
                }
                l.wrapping_shr(r as u32)
            },
            "&&" => i128::from(l != 0 && r != 0),
            "||" => i128::from(l != 0 || r != 0),
            "==" => i128::from(l == r),
            "!=" => i128::from(l != r),
            "<" => i128::from(l < r),
            ">" => i128::from(l > r),
            "<=" => i128::from(l <= r),
            ">=" => i128::from(l >= r),
            _ => return None,
        })
    }

    fn parse_unary(&mut self) -> Option<i128> {
        let op = match self.peek_tok() {
            Some(ConstTok::Punct(p)) if matches!(p, "+" | "-" | "~" | "!") => p,
            _ => return self.parse_primary(),
        };
        self.pos += 1;
        let v = self.parse_unary()?;
        Some(match op {
            "+" => v,
            "-" => v.wrapping_neg(),
            "~" => !v,
            _ => i128::from(v == 0),
        })
    }

    fn parse_primary(&mut self) -> Option<i128> {
        let tok = self.peek_tok()?;
        match tok {
            ConstTok::Num(v) => {
                self.pos += 1;
                Some(v)
            },
            ConstTok::Ident(name) => {
                self.pos += 1;
                self.env.get(name).copied()
            },
            ConstTok::Punct("(") => {
                self.pos += 1;
                let v = self.parse_binary(0)?;
                if !self.eat_punct(")") {
                    return None;
                }
                Some(v)
            },
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, i128)]) -> HashMap<String, i128> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn integer_literal_radixes_suffixes_and_separators() {
        assert_eq!(parse_integer_literal_text("42"), Some(42));
        assert_eq!(parse_integer_literal_text(" 0x1F "), Some(31));
        assert_eq!(parse_integer_literal_text("0B1010"), Some(10));
        assert_eq!(parse_integer_literal_text("010"), Some(8));
        assert_eq!(parse_integer_literal_text("1'000"), Some(1000));
        assert_eq!(parse_integer_literal_text("7u"), Some(7));
        assert_eq!(parse_integer_literal_text("8UL"), Some(8));
        assert_eq!(parse_integer_literal_text("0"), Some(0));
    }

    #[test]
    fn integer_literal_rejects_non_literals() {
        assert_eq!(parse_integer_literal_text(""), None);
        assert_eq!(parse_integer_literal_text("A"), None);
        assert_eq!(parse_integer_literal_text("0xZZ"), None);
        assert_eq!(parse_integer_literal_text("1.5"), None);
        assert_eq!(parse_integer_literal_text("12ab"), None);
        assert_eq!(parse_integer_literal_text("u"), None);
    }

    #[test]
    fn enum_style_initializers_fold_with_cpp_precedence() {
        let e = env(&[("A", 1), ("FLAG", 4)]);
        assert_eq!(eval_integer_const_text("A + 2", &e), Some(3));
        assert_eq!(eval_integer_const_text("1 << 3", &e), Some(8));
        assert_eq!(eval_integer_const_text("(1 | FLAG) + 1", &e), Some(6));
        assert_eq!(eval_integer_const_text("1 | FLAG + 1", &e), Some(5));
        assert_eq!(eval_integer_const_text("-A", &e), Some(-1));
        assert_eq!(eval_integer_const_text("~0", &e), Some(-1));
        assert_eq!(eval_integer_const_text("2 + 3 * 4", &e), Some(14));
        assert_eq!(eval_integer_const_text("10 / 2 % 3", &e), Some(2));
        assert_eq!(eval_integer_const_text("0x10 >> 2", &e), Some(4));
        assert_eq!(eval_integer_const_text("'A'", &e), Some(65));
        assert_eq!(eval_integer_const_text("'\\n'", &e), Some(10));
        assert_eq!(eval_integer_const_text("(A)", &e), Some(1));
        assert_eq!(eval_integer_const_text("1 < 2", &e), Some(1));
        assert_eq!(eval_integer_const_text("!A", &e), Some(0));
    }

    #[test]
    fn non_constant_expressions_are_rejected() {
        let e = env(&[("A", 1)]);
        // 未知标识符 / 函数调用 / sizeof / 拖尾垃圾
        assert_eq!(eval_integer_const_text("UNKNOWN", &e), None);
        assert_eq!(eval_integer_const_text("f(1)", &e), None);
        assert_eq!(eval_integer_const_text("sizeof(int)", &e), None);
        assert_eq!(eval_integer_const_text("1 2", &e), None);
        assert_eq!(eval_integer_const_text("(1", &e), None);
        assert_eq!(eval_integer_const_text("", &e), None);
        assert_eq!(eval_integer_const_text("A :: B", &e), None);
    }

    /// 字符串字面量转义解码 ([lex.escape]) —— native 路径上字符串
    /// 字面量不再被占位成整数 0, 必须能真实落 rodata, 这里是它的解码真源。
    #[test]
    fn string_escapes_decode_to_real_bytes() {
        assert_eq!(decode_string_escapes("abc"), b"abc".to_vec());
        assert_eq!(decode_string_escapes("a\\nb"), vec![b'a', b'\n', b'b']);
        assert_eq!(decode_string_escapes("\\t\\r\\a\\b\\f\\v"), vec![9, 13, 7, 8, 12, 11]);
        assert_eq!(decode_string_escapes("\\\\\\'\\\"\\?"), vec![b'\\', b'\'', b'"', b'?']);
        // 八进制最多 3 位; 后面的普通字符不再属于转义
        assert_eq!(decode_string_escapes("\\101B"), vec![b'A', b'B']);
        assert_eq!(decode_string_escapes("\\1109"), vec![b'H', b'9']);
        // 十六进制吸收全部连续十六进制位, 按 u8 模截 (与 GCC 一致)
        assert_eq!(decode_string_escapes("\\x41"), vec![b'A']);
        assert_eq!(decode_string_escapes("\\x41z"), vec![0x41, b'z']);
        assert_eq!(decode_string_escapes("\\x141"), vec![0x41]);
        // 空串 / 非转义反斜杠开头的未知转义: 丢反斜杠保留后一字符
        assert!(decode_string_escapes("").is_empty());
        assert_eq!(decode_string_escapes("\\q"), vec![b'q']);
    }

    /// 字符字面量与字符串字面量共用同一个转义解码器 (单一真源):
    /// `'\\101'` / `'\\x41'` / `'A'` 必须同值。
    #[test]
    fn char_literal_shares_the_string_escape_decoder() {
        let e = env(&[]);
        assert_eq!(eval_integer_const_text("'A'", &e), Some(65));
        assert_eq!(eval_integer_const_text("'\\101'", &e), Some(65));
        assert_eq!(eval_integer_const_text("'\\x41'", &e), Some(65));
        assert_eq!(eval_integer_const_text("'\\n'", &e), Some(10));
        assert_eq!(eval_integer_const_text("'\\\\'", &e), Some(92));
        assert_eq!(eval_integer_const_text("'\\''", &e), Some(39));
    }
}
