//! Itanium ABI 的**类型编码** (`<type>` 产生式) 与 typeinfo 符号名。
//!
//! 为什么单独成模块: zero-cost EH 的类型匹配完全由 `_ZTI<encoding>` 这一个
//! 符号决定 —— `__cxa_throw` 的第二参数、LSDA 类型表的每一项、以及
//! `typeid` 的静态路径都必须给出**与 g++ 逐字节一致**的编码, 否则
//! `__class_type_info::__do_catch` 里的 `strcmp(__name)` 永远不会相等, 表现成
//! 「异常抛出了却捕获不到」。类类型的编码已由布局层 (`ClassAbi::rtti`) 依
//! 限定名算出, 本模块补齐**布局层覆盖不到**的那一半: 基本类型与指针。
//!
//! 发射侧: `native.rs` 按 [`builtin_typeinfo_symbols`] 的结果发 `_ZTS`/`_ZTI`
//! 两个 weak 对象 (基本类型用 `__class_type_info` 形态, 即两字: vptr + name)。

use crate::cpp::sema::CxxType;

/// 基本类型的 Itanium 缩写 (ABI 附录 "Builtin types" 表)。
///
/// 注意 `char` / `signed char`: dcc 的类型系统把两者都压成
/// `Integer { bits: 8, signed: true }`, 因此这里统一发 `c`。这与编译器其余
/// 部分对 `char` 的处理一致 (重载决议同样区分不了它们), 后果只在
/// 「同时存在 `catch (char)` 与 `catch (signed char)` 两个分支」这种本身就
/// 不该出现在合法程序里的写法上。
fn builtin_abbrev(ty: &CxxType) -> Option<&'static str> {
    match ty {
        CxxType::Void => Some("v"),
        CxxType::Bool => Some("b"),
        CxxType::Integer {
            bits: 8,
            signed: true,
        } => Some("c"),
        CxxType::Integer {
            bits: 8,
            signed: false,
        } => Some("h"),
        CxxType::Integer {
            bits: 16,
            signed: true,
        } => Some("s"),
        CxxType::Integer {
            bits: 16,
            signed: false,
        } => Some("t"),
        CxxType::Integer {
            bits: 32,
            signed: true,
        } => Some("i"),
        CxxType::Integer {
            bits: 32,
            signed: false,
        } => Some("j"),
        CxxType::Integer {
            bits: 64,
            signed: true,
        } => Some("l"),
        CxxType::Integer {
            bits: 64,
            signed: false,
        } => Some("m"),
        CxxType::Integer { bits: 128, .. } => None,
        CxxType::Float { bits: 32 } => Some("f"),
        CxxType::Float { bits: 64 } => Some("d"),
        CxxType::Float { bits: 80 } | CxxType::Float { bits: 128 } => Some("e"),
        CxxType::Float { .. } => None,
        _ => None,
    }
}

/// 一个**非类**类型的 Itanium `<type>` 编码。
///
/// 覆盖 `throw` / `catch` 实际能拿到的完整对象类型: 基本类型、指针、以及
/// 它们的组合 (`**char` → `PPc`)。类类型不在这里 (由布局层给), 数组/函数
/// 类型不能成为 catch 类型 (catch 会退化为指针), 引用同理。
pub fn encode_non_class_type(ty: &CxxType) -> Option<String> {
    match ty {
        CxxType::Enum(_) => {
            // 枚举的底层表示是整型, 但 ABI 编码用枚举名。 dcc 的枚举没有独立
            // 的 mangled 名可用 (`ClassId` 只带类名表), 交给调用方按类名路径处理。
            None
        },
        CxxType::Pointer(inner) => {
            let base = encode_non_class_type(inner)?;
            Some(format!("P{}", base))
        },
        other => builtin_abbrev(other).map(|s| s.to_string()),
    }
}

/// 一个非类类型的 `_ZTI` / `_ZTS` 符号名对。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinTypeinfoSymbols {
    /// `_ZTI<encoding>` —— `__cxa_throw` 与 LSDA 类型表引用的 typeinfo 对象
    pub typeinfo_symbol: String,
    /// `_ZTS<encoding>` —— typeinfo 对象的 `__name` 字段
    pub name_symbol: String,
    /// `<encoding>` 本身, 即 `_ZTS` 字符串的内容 (含结尾 NUL 由发射侧补)
    pub encoding: String,
}

/// 该类型是否需要**由本模块合成**的 typeinfo (非类且能编码)。
pub fn builtin_typeinfo_symbols(ty: &CxxType) -> Option<BuiltinTypeinfoSymbols> {
    let encoding = encode_non_class_type(ty)?;
    Some(named_typeinfo_symbols(&encoding))
}

/// 直接按已 mangle 好的类型编码合成符号对。
///
/// 给「编码不是从类型结构推出、而要由名字现算」的那一类使用: 枚举的 ABI 编码
/// 是它的**限定名** (`_ZTI1E`), 但 libstdc++ 的 `__do_catch` 只比 `__name` 字符串,
/// 所以用 `__class_type_info` 形态发两个字 (vptr + name) 就足够参与异常匹配。
pub fn named_typeinfo_symbols(encoding: &str) -> BuiltinTypeinfoSymbols {
    BuiltinTypeinfoSymbols {
        typeinfo_symbol: format!("_ZTI{}", encoding),
        name_symbol: format!("_ZTS{}", encoding),
        encoding: encoding.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(bits: u16, signed: bool) -> CxxType {
        CxxType::Integer { bits, signed }
    }

    #[test]
    fn fundamental_encodings_match_itanium_table() {
        assert_eq!(encode_non_class_type(&CxxType::Void).as_deref(), Some("v"));
        assert_eq!(encode_non_class_type(&CxxType::Bool).as_deref(), Some("b"));
        assert_eq!(encode_non_class_type(&int(8, true)).as_deref(), Some("c"));
        assert_eq!(encode_non_class_type(&int(8, false)).as_deref(), Some("h"));
        assert_eq!(encode_non_class_type(&int(16, true)).as_deref(), Some("s"));
        assert_eq!(encode_non_class_type(&int(32, true)).as_deref(), Some("i"));
        assert_eq!(encode_non_class_type(&int(32, false)).as_deref(), Some("j"));
        assert_eq!(encode_non_class_type(&int(64, true)).as_deref(), Some("l"));
        assert_eq!(encode_non_class_type(&int(64, false)).as_deref(), Some("m"));
        assert_eq!(encode_non_class_type(&CxxType::Float { bits: 32 }).as_deref(), Some("f"));
        assert_eq!(encode_non_class_type(&CxxType::Float { bits: 64 }).as_deref(), Some("d"));
    }

    #[test]
    fn pointer_encodings_nest_and_class_types_are_rejected() {
        let char_p = CxxType::Pointer(Box::new(int(8, true)));
        assert_eq!(encode_non_class_type(&char_p).as_deref(), Some("Pc"));
        let char_pp = CxxType::Pointer(Box::new(char_p));
        assert_eq!(encode_non_class_type(&char_pp).as_deref(), Some("PPc"));
        // 类类型必须走布局层 (带限定名的 `_ZTI`), 这里不能给出编码
        assert_eq!(encode_non_class_type(&CxxType::Class(crate::cpp::sema::ClassId(3))), None);
    }

    #[test]
    fn symbol_pair_is_consistent_with_encoding() {
        let s = builtin_typeinfo_symbols(&int(32, true)).expect("int 应可编码");
        assert_eq!(s.encoding, "i");
        assert_eq!(s.typeinfo_symbol, "_ZTIi");
        assert_eq!(s.name_symbol, "_ZTSi");
        // 枚举路径: 编码由限定名现算, 符号对同样成立
        let e = named_typeinfo_symbols("1E");
        assert_eq!(e.typeinfo_symbol, "_ZTI1E");
        assert_eq!(e.name_symbol, "_ZTS1E");
    }
}
