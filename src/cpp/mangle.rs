//! Itanium C++ ABI 名字修饰 (M7 最小).
//!
//! 完整 Itanium ABI 见 Itanium C++ ABI 5.1.  本模块实现 M7 范围
//! 必需的子集: 类名, 成员函数, 构造, 析构.  模板 / 虚函数表 /
//! typeinfo 等用到的 RTTI 符号暂不实现 (GCC bootstrap 阶段 1
//! 通常不会立刻触雷, 但 stage 2/3 会需要).
//!
//! 修饰格式 (简化):
//!   _ZN<name>E                   -- 嵌套名
//!   _ZN<len><class>E<name>E      -- 限定名
//!   _ZN<len><class>E<name>...    -- 成员函数 (含参数)
//!   C1                           -- 完整对象构造 (base ctor)
//!   C2                           -- 基对象构造 (base ctor)
//!   C3                           -- 完整分配构造 (allocating ctor)
//!   D1                           -- 完整对象析构 (base dtor)
//!   D2                           -- 基对象析构 (base dtor)
//!
//! 例子:
//!   Foo::bar()                 -> _ZN3Foo3barEv
//!   Foo::Foo(int)              -> _ZN3FooC1Ei
//!   Foo::~Foo()                -> _ZN3FooD1Ev

/// 类型编码 (M7 范围: 内置类型 + 类引用)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MangleType {
    Void,
    Bool,
    Char,
    Schar,
    Uchar,
    Short,
    Ushort,
    Int,
    Uint,
    Long,
    Ulong,
    LongLong,
    UlongLong,
    Float,
    Double,
    LongDouble,
    Pointer(Box<MangleType>),
    Reference(Box<MangleType>),
    Class(String),
    Ellipsis,
}

impl MangleType {
    /// 写出 Itanium 类型编码 (不含 <len> 前缀)
    pub fn encode(&self, out: &mut String) {
        match self {
            Self::Void => out.push('v'),
            Self::Bool => out.push('b'),
            Self::Char => out.push('c'),
            Self::Schar => out.push('a'),
            Self::Uchar => out.push('h'),
            Self::Short => out.push('s'),
            Self::Ushort => out.push('t'),
            Self::Int => out.push('i'),
            Self::Uint => out.push('j'),
            Self::Long => out.push('l'),
            Self::Ulong => out.push('m'),
            Self::LongLong => out.push('x'),
            Self::UlongLong => out.push('y'),
            Self::Float => out.push('f'),
            Self::Double => out.push('d'),
            Self::LongDouble => out.push('e'),
            Self::Pointer(t) => { out.push('P'); t.encode(out); }
            Self::Reference(t) => { out.push('R'); t.encode(out); }
            Self::Class(n) => { out.push_str(&format!("{}N{}E", n.len(), n)); }
            Self::Ellipsis => out.push('z'),
        }
    }
}

/// 限定名片段 (按从外到内顺序)
#[derive(Debug, Clone)]
pub struct NestedName {
    pub parts: Vec<String>,
}

impl NestedName {
    pub fn from_dotted(parts: Vec<String>) -> Self { Self { parts } }

    /// 编码成 _ZN<part1_len><part1>...<partN_len><partN>E
    pub fn encode(&self, out: &mut String) {
        out.push_str("_ZN");
        for p in &self.parts {
            out.push_str(&format!("{}{}", p.len(), p));
        }
        out.push('E');
    }

    /// 编码成 _ZN<part1_len><part1>...<partN_len><partN> (不带尾 E)
    /// 用于 ctor/dtor 场景, 因为它们自带 C1/D1 后缀
    pub fn encode_no_trailing_e(&self, out: &mut String) {
        out.push_str("_ZN");
        for p in &self.parts {
            out.push_str(&format!("{}{}", p.len(), p));
        }
    }
}

/// 成员函数修饰 (完整名, 含参数与返回)
/// 格式: _ZN<name>E<bare-function-type>
/// bare-function-type = 形参类型列表 (空参时为 v)
pub fn mangle_member_function(
    name: &NestedName,
    params: &[MangleType],
    _ret: &MangleType,
) -> String {
    let mut out = String::new();
    name.encode(&mut out);
    if params.is_empty() {
        out.push('v');
    } else {
        for p in params {
            p.encode(&mut out);
        }
    }
    out
}

/// 构造函数修饰
/// ctor_kind: 1=base ctor, 2=base ctor with args, 3=allocating
/// 格式: _ZN<name>C1E<bare-function-type>
/// 注: C1 是 <unqualified-name> 的一部分, 后面要跟 E 闭合 nested-name
pub fn mangle_ctor(name: &NestedName, params: &[MangleType], ctor_kind: u8) -> String {
    let mut out = String::new();
    name.encode_no_trailing_e(&mut out);
    out.push('C');
    out.push((b'0' + ctor_kind) as char);
    out.push('E'); // 闭合 nested-name
    if params.is_empty() {
        out.push('v');
    } else {
        for p in params {
            p.encode(&mut out);
        }
    }
    out
}

/// 析构函数修饰
/// dtor_kind: 1=base dtor, 2=deleting dtor
/// 格式: _ZN<name>D1E<bare-function-type>
pub fn mangle_dtor(name: &NestedName, dtor_kind: u8) -> String {
    let mut out = String::new();
    name.encode_no_trailing_e(&mut out);
    out.push('D');
    out.push((b'0' + dtor_kind) as char);
    out.push('E'); // 闭合 nested-name
    out.push('v');
    out
}

/// 全局函数 (无类作用域) 修饰: _Z<name><args>E
/// 注: 全局函数尾 E 是闭合整个 name (包括 args)
pub fn mangle_global_function(name: &str, params: &[MangleType]) -> String {
    let mut out = String::new();
    out.push_str("_Z");
    out.push_str(&format!("{}{}", name.len(), name));
    if params.is_empty() {
        out.push('v');
    } else {
        for p in params {
            p.encode(&mut out);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn t_mangle_void_void() {
        // M7: 空参函数 = 编码为 `v` (void arg, no trailing E)
        let n = NestedName::from_dotted(vec!["Foo".into(), "bar".into()]);
        let m = mangle_member_function(&n, &[], &MangleType::Void);
        assert_eq!(m, "_ZN3Foo3barEv");
    }
    #[test]
    fn mangle_foo_bar_int() {
        let n = NestedName::from_dotted(vec!["Foo".into(), "bar".into()]);
        assert_eq!(
            mangle_member_function(&n, &[MangleType::Int], &MangleType::Void),
            "_ZN3Foo3barEi"
        );
    }
    #[test]
    fn t_mangle_ctor() {
        let n = NestedName::from_dotted(vec!["Foo".into()]);
        assert_eq!(mangle_ctor(&n, &[MangleType::Int], 1), "_ZN3FooC1Ei");
    }
    #[test]
    fn t_mangle_dtor() {
        let n = NestedName::from_dotted(vec!["Foo".into()]);
        assert_eq!(mangle_dtor(&n, 1), "_ZN3FooD1Ev");
    }
    #[test]
    fn mangle_pointer_type() {
        let n = NestedName::from_dotted(vec!["Foo".into(), "bar".into()]);
        assert_eq!(
            mangle_member_function(&n, &[MangleType::Pointer(Box::new(MangleType::Int))], &MangleType::Void),
            "_ZN3Foo3barEPi"
        );
    }
    #[test]
    fn mangle_reference_type() {
        let n = NestedName::from_dotted(vec!["Foo".into(), "bar".into()]);
        assert_eq!(
            mangle_member_function(&n, &[MangleType::Reference(Box::new(MangleType::Int))], &MangleType::Void),
            "_ZN3Foo3barERi"
        );
    }
    #[test]
    fn mangle_global() {
        assert_eq!(
            mangle_global_function("foo", &[MangleType::Int, MangleType::Int]),
            "_Z3fooii"
        );
    }
}
