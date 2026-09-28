//! Itanium C++ ABI 名字修饰 (M7 + M8 扩展).
//!
//! 实现 Itanium C++ ABI 5.1 中 M7/M8 必需的子集:
//!   - 类名 / 限定名 / 嵌套名
//!   - 构造 (C1/C2/C3) / 析构 (D0/D1/D2)
//!   - 运算符重载 (operator+/-/*/.../()/<</>>/==/...)
//!   - 模板 (M8: 留作模板参数后缀)
//!   - vtable / typeinfo (M8: _ZTV / _ZTI)
//!
//! 修饰格式 (简化):
//!   _ZN<name>E                   -- 嵌套名
//!   _ZN<len><class>E<name>E      -- 限定名
//!   _ZN<len><class>E<name>...    -- 成员函数 (含参数)
//!   C1 / C2 / C3                 -- 构造
//!   D0 / D1 / D2                 -- 析构
//!   _ZTV<name>                   -- vtable
//!   _ZTI<name>                   -- typeinfo
//!
//! 例子:
//!   Foo::bar()                 -> _ZN3Foo3barEv
//!   Foo::Foo(int)              -> _ZN3FooC1Ei
//!   Foo::~Foo()                -> _ZN3FooD1Ev
//!   Foo::operator+(int)        -> _ZN3FooaddEi (M8 简化为 add/sub/mul/..)
//!   vtable for Foo             -> _ZTV3Foo
//!   typeinfo for Foo           -> _ZTI3Foo

use crate::tokens::TokKind;

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
    /// 定长数组 `T[N]`（元素类型 + 元素个数）。
    /// 用于局部/全局数组声明的语义化，native 后端据此分配栈/静态存储。
    Array(Box<MangleType>, usize),
    /// 函数类型 `R(Args...)`。**只能出现在 `Pointer(...)` 里**（C++ 中
    /// 除声明/定义外没处能放一个函数对象），用法是
    /// `Pointer(Box::new(MangleType::Function{..}))` 表示 `R(*)(Args...)`。
    ///
    /// 为什么需要它（而不是把 `T (*f)(U)` 近似成 `Pointer(T)`）：
    /// native sema 的重载决议要把实参类型和形参类型逐字段比等。标准库的
    /// manipulator 重载（`istream& operator>>(istream& (*)(istream&))`、
    /// `ostream& operator<<(ostream& (*)(ostream&))`）的形参是**函数指针**，
    /// 丢了 `(U)` 部分后类型永远不相等，候选全部不可行，`is >> std::ws`
    /// 会退化成**内建 `>>`**（对流对象做 `shr`），得到垃圾 `this` → 运行期段错。
    Function {
        ret: Box<MangleType>,
        params: Vec<MangleType>,
    },
    Class(String),
    Ellipsis,
    /// `auto` 占位 - 由 lower 阶段基于 init 表达式推导真实类型
    Auto,
    /// `decltype(auto)` 占位 - 规则同 C++14: 用 decltype(expr) 推导,
    /// 保留引用与 cv 限定. 由 lower 阶段完成.
    DecltypeAuto,
    /// 模板实例化: `T<U1, U2, ...>` 解析为 `Template { name: "T", args: [...] }`.
    /// Itanium 编码: `N<name_len><name>I<arg1><arg2>...E` (与 St6vectorIiSaIiEE 形式一致).
    Template {
        name: String,
        args: Vec<MangleType>,
    },
    /// 类型语境的模板类 typedef 成员引用 `Tpl<Args>::member`
    /// (如 std::underlying_type<E>::type / std::conditional<C,T,F>::type).
    /// 成员必须是模板类内的 typedef; lower 阶段展开为实例类中该
    /// typedef 形参替换后的真实类型 (见 Lower::inst_typedefs).
    TplMember {
        name: String,
        args: Vec<MangleType>,
        member: String,
    },
    /// M30.1: 非类型模板实参 (`conditional<false, T, F>` 的 `false`,
    /// `arr<int, 16>` 的 `16`). 编译期常量，不作为 C 类型出现;
    /// 参与实例 key / 偏特化匹配。Itanium 编码 `Li<val>E`.
    NonType(i64),
    /// 可变参数模板的参数包展开 (C++11 variadic templates).
    /// 表示一组类型的 pack，用于 `std::tuple<int, double, char>` 等.
    /// Itanium 编码时作为多个连续参数处理.
    VariadicPack(Vec<MangleType>),
    /// G11: 模板模板参数 - 指向被传递的模板定义
    /// 用于 `template<typename T, template<typename> class Container> Adapter<T, Container>`
    /// 中的 `Container` 形参，实际传入时绑定到 `std::vector` 等模板符号
    TemplateParam(SymbolId),
    /// [temp.dep.constexpr]: 模板**实参位置**的依赖常量表达式。
    ///
    /// `enable_if<!is_pointer<T>::value, int>` 的首实参含模板形参, 解析期求不出值;
    /// 旧实现直接报 Err, 整条声明被顶层容错降级成 Opaque。现在把子式按结构保留
    /// (`op` + 递归 `operands`, 叶子是 `NonType` / `Class(形参名)` / `TplMember`),
    /// 单态化把形参代入为具体实参后由 `TemplateRegistry::fold_args` 折成 `NonType`。
    /// 只能是**类型**的表达式 (如取地址、函数调用) 不进这里 —— 那不是合法的
    /// 非类型实参, 保留解析期硬错。
    NtExpr(NtOp, Vec<MangleType>),
}

/// [`MangleType::NtExpr`] 的运算符。枚举而不是文本: 两个实例是否同一表达式
/// 取决于**运算语义** (`a<b` 与 `a< b` 是同一个), 文本比较会把它们当成两个。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NtOp {
    /// `!x`
    LNot,
    /// `~x`
    Not,
    /// `-x` (一元负)
    Neg,
    /// `a + b`
    Add,
    /// `a - b`
    Sub,
    /// `a * b`
    Mul,
    /// `a / b`
    Div,
    /// `a % b`
    Mod,
    /// `a << b`
    Shl,
    /// `a >> b`
    Shr,
    /// `a < b`
    Lt,
    /// `a <= b`
    Le,
    /// `a > b`
    Gt,
    /// `a >= b`
    Ge,
    /// `a == b`
    Eq,
    /// `a != b`
    Ne,
    /// `a && b`
    Land,
    /// `a || b`
    Lor,
    /// `a & b`
    BitAnd,
    /// `a | b`
    BitOr,
    /// `a ^ b`
    BitXor,
    /// `cond ? a : b` (三个操作数)
    Cond,
    /// `sizeof(T)` (操作数是类型)
    Sizeof,
}

/// G11: 模板模板参数符号 ID - 引用一个模板名称
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolId(pub String);

impl NtOp {
    /// 运算符的稳定短名 (参与实例 key / C 标识符片段 / 符号唯一性编码)
    pub fn key(self) -> &'static str {
        match self {
            NtOp::LNot => "lnot",
            NtOp::Not => "not",
            NtOp::Neg => "neg",
            NtOp::Add => "add",
            NtOp::Sub => "sub",
            NtOp::Mul => "mul",
            NtOp::Div => "div",
            NtOp::Mod => "mod",
            NtOp::Shl => "shl",
            NtOp::Shr => "shr",
            NtOp::Lt => "lt",
            NtOp::Le => "le",
            NtOp::Gt => "gt",
            NtOp::Ge => "ge",
            NtOp::Eq => "eq",
            NtOp::Ne => "ne",
            NtOp::Land => "land",
            NtOp::Lor => "lor",
            NtOp::BitAnd => "band",
            NtOp::BitOr => "bor",
            NtOp::BitXor => "bxor",
            NtOp::Cond => "cond",
            NtOp::Sizeof => "sizeoff",
        }
    }

    /// 该运算符期望的操作数个数 (`Cond` 三个, `Sizeof` 一个, 二元两个, 一元一个)
    pub fn arity(self) -> usize {
        match self {
            NtOp::Cond => 3,
            NtOp::LNot | NtOp::Not | NtOp::Neg | NtOp::Sizeof => 1,
            _ => 2,
        }
    }
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
            Self::Pointer(t) => {
                out.push('P');
                t.encode(out);
            },
            Self::Reference(t) => {
                out.push('R');
                t.encode(out);
            },
            // Itanium: A<n>_<element>
            Self::Array(t, n) => {
                out.push('A');
                out.push_str(&n.to_string());
                out.push('_');
                t.encode(out);
            },
            // Itanium: 函数类型 = `F <d-type> E <p-type>* E`，所以指针指向
            // 函数 (`Pointer(Function)`) 自然编出 `PF...E...E`，与 GCC 对
            // `R(*)(Args)` 的编码一致。
            Self::Function { ret, params } => {
                out.push('F');
                ret.encode(out);
                out.push('E');
                for p in params {
                    p.encode(out);
                }
                out.push('E');
            },
            Self::Class(n) => {
                out.push_str(&format!("{}N{}E", n.len(), n));
            },
            Self::Ellipsis => out.push('z'),
            // G11: TemplateParam - encode as class-like reference
            Self::TemplateParam(sym) => {
                out.push_str(&format!("{}N{}E", sym.0.len(), sym.0));
            },
            // M17.0: Auto / DecltypeAuto 由 lower 阶段解析, 不进入 mangle.
            // 显式抛错: 若某条路径未完成 auto 推导, 编译时报错而非输出错符号.
            Self::Auto => out.push_str("__auto_unresolved__"),
            Self::DecltypeAuto => out.push_str("__decltype_auto_unresolved__"),
            // 模板实例化: N<name_len><name>I<arg1>...<argN>E
            // 与 St6vectorIiSaIiEE 一致
            Self::Template { name, args } => {
                out.push_str(&format!("N{}{}I", name.len(), name));
                for a in args {
                    a.encode(out);
                }
                out.push('E');
            },
            Self::TplMember { name, args, member } => {
                out.push_str(&format!("N{}{}I", name.len(), name));
                for a in args {
                    a.encode(out);
                }
                out.push('E');
                out.push_str(&format!("{}{}", member.len(), member));
            },
            // M30.1: 非类型实参编码成整数字面量 (Li1E / LiN3E),
            // 与 GCC `int_const` 形式的 Itanium 编码一致.
            Self::NonType(v) => {
                if *v < 0 {
                    out.push_str(&format!("LiN{}E", -*v));
                } else {
                    out.push_str(&format!("Li{}E", v));
                }
            },
            // G12: 参数包展开 - 依次编码包内的每个类型
            Self::VariadicPack(packed) => {
                for ty in packed {
                    ty.encode(out);
                }
            },
            // [temp.dep.constexpr]: 依赖非类型子式。真正的 Itanium 编码把折叠后的
            // 常量写成 `Li<值>E`; 未折叠时只能按结构编码, 目的是**符号唯一性**
            // (同一表达式两个实例必须拿到同一个名字, 不同表达式必须不同)。
            Self::NtExpr(op, operands) => {
                out.push_str(&format!("2{}", op.key()));
                for a in operands {
                    a.encode(out);
                }
            },
        }
    }

    /// 推断类型是否"待解析" (M17.0: auto / decltype(auto) 在 lower 阶段解析)
    pub fn is_unresolved(&self) -> bool {
        match self {
            Self::Auto | Self::DecltypeAuto => true,
            // G11: TemplateParam already bound to concrete template
            Self::TemplateParam(_) => false,
            // G12: 参数包中的子类型也需要检查
            Self::VariadicPack(packed) => packed.iter().any(|t| t.is_unresolved()),
            _ => false,
        }
    }
}

/// 限定名片段 (按从外到内顺序)
#[derive(Debug, Clone)]
pub struct NestedName {
    pub parts: Vec<String>,
}

impl NestedName {
    pub fn from_dotted(parts: Vec<String>) -> Self {
        Self { parts }
    }

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

/// 全局变量 (命名空间作用域, 无类作用域) 修饰: _Z<name>
///
/// Itanium ABI 里变量不参与重载, 因此 <unqualified-name> 之后不跟类型编码
/// (只有静态数据成员才追加 <d-name> 之外的类型)。M28.6 用它做模块导出
/// 变量的发射符号名, 与同名全局函数 / 类成员区分。
pub fn mangle_global_variable(name: &str) -> String {
    format!("_Z{}{}", name.len(), name)
}

/// 运算符 (二元 / 一元 / 转型 / 函数调用)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operator {
    // 二元算术
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    // 二元位运算
    Shl,
    Shr,
    And,
    Or,
    Xor,
    // 比较
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    // 逻辑
    LAnd,
    LOr,
    // 赋值
    Assign,
    AddAssign,
    SubAssign,
    MulAssign,
    DivAssign,
    ModAssign,
    ShlAssign,
    ShrAssign,
    AndAssign,
    OrAssign,
    XorAssign,
    // 自增自减
    Inc,
    Dec,
    // 复合下标 / 调用 / 成员
    Index,
    Call,
    Arrow,
    ArrowStar,
    // 一元
    Plus,
    Minus,
    Not,
    LNot,
    Deref,
    AddrOf,
    PreInc,
    PreDec,
    PostInc,
    PostDec,
    // 转型
    CastTo(MangleType),
    // 特殊
    Comma,
    // M26.1: 分配 / 释放运算符 (类成员 operator new/delete)
    New,
    Delete,
    NewArr,
    DeleteArr,
}

impl Operator {
    /// 编码成 Itanium operator-name (简单形式: 用字符串代替)
    pub fn encode_name(&self) -> &'static str {
        match self {
            Self::Add => "pl",
            Self::Sub => "mi",
            Self::Mul => "ml",
            Self::Div => "dv",
            Self::Mod => "rm",
            Self::Shl => "ls",
            Self::Shr => "rs",
            Self::And => "an",
            Self::Or => "or",
            Self::Xor => "eo",
            Self::Eq => "eq",
            Self::Ne => "ne",
            Self::Lt => "lt",
            Self::Le => "le",
            Self::Gt => "gt",
            Self::Ge => "ge",
            Self::LAnd => "aa",
            Self::LOr => "oo",
            Self::Assign => "aS",
            Self::AddAssign => "pL",
            Self::SubAssign => "mI",
            Self::MulAssign => "mL",
            Self::DivAssign => "dV",
            Self::ModAssign => "rM",
            Self::ShlAssign => "lS",
            Self::ShrAssign => "rS",
            Self::AndAssign => "aN",
            Self::OrAssign => "oR",
            Self::XorAssign => "eO",
            Self::Inc | Self::PreInc | Self::PostInc => "pp",
            Self::Dec | Self::PreDec | Self::PostDec => "mm",
            Self::Index => "ix",
            Self::Call => "cl",
            Self::Arrow => "pt",
            Self::ArrowStar => "pm",
            Self::Plus => "ps",
            Self::Minus => "ng",
            Self::Not => "co",
            Self::LNot => "nt",
            Self::Deref => "de",
            Self::AddrOf => "ad",
            Self::Comma => "cm",
            Self::CastTo(_) => "cv",
            // M26.1: Itanium ABI operator new/delete 编码
            Self::New => "nw",
            Self::Delete => "dl",
            Self::NewArr => "na",
            Self::DeleteArr => "da",
        }
    }

    /// 把 + - * / ++ -- 等原始运算符字面量映射到 Operator
    pub fn from_token(s: &str) -> Option<Self> {
        Some(match s {
            "+" => Self::Add,
            "-" => Self::Sub,
            "*" => Self::Mul,
            "/" => Self::Div,
            "%" => Self::Mod,
            "<<" => Self::Shl,
            ">>" => Self::Shr,
            "&" => Self::And,
            "|" => Self::Or,
            "^" => Self::Xor,
            "==" => Self::Eq,
            "!=" => Self::Ne,
            "<" => Self::Lt,
            "<=" => Self::Le,
            ">" => Self::Gt,
            ">=" => Self::Ge,
            "&&" => Self::LAnd,
            "||" => Self::LOr,
            "=" => Self::Assign,
            "+=" => Self::AddAssign,
            "-=" => Self::SubAssign,
            "*=" => Self::MulAssign,
            "/=" => Self::DivAssign,
            "%=" => Self::ModAssign,
            "<<=" => Self::ShlAssign,
            ">>=" => Self::ShrAssign,
            "&=" => Self::AndAssign,
            "|=" => Self::OrAssign,
            "^=" => Self::XorAssign,
            "++" => Self::Inc,
            "--" => Self::Dec,
            // 一元 `!` / `~` (字面量组合路径, 与 from_tok_kind 保持一致)
            "!" => Self::LNot,
            "~" => Self::Not,
            "()" => Self::Call,
            "[]" => Self::Index,
            "->" => Self::Arrow,
            "->*" => Self::ArrowStar,
            "," => Self::Comma,
            _ => return None,
        })
    }
}

/// 把 lexer 输出的 TokKind 直接映射到 Operator (M11 stdlib).
/// lexer 会把 `+=` 合并成 `PlusEq` 等, 因此按字符字面量匹配不可靠.
/// 这里按 TokKind 一对一映射, 避免重复消费.
pub fn from_tok_kind(kind: &TokKind) -> Option<Operator> {
    use Operator as Op;
    Some(match kind {
        TokKind::Plus => Op::Add,
        TokKind::Minus => Op::Sub,
        TokKind::Star => Op::Mul,
        TokKind::Slash => Op::Div,
        TokKind::Percent => Op::Mod,
        TokKind::Shl => Op::Shl,
        TokKind::Shr => Op::Shr,
        TokKind::Amp => Op::And,
        TokKind::Pipe => Op::Or,
        TokKind::Caret => Op::Xor,
        TokKind::EqEq => Op::Eq,
        TokKind::Ne => Op::Ne,
        TokKind::Lt => Op::Lt,
        TokKind::Le => Op::Le,
        TokKind::Gt => Op::Gt,
        TokKind::Ge => Op::Ge,
        TokKind::AndAnd => Op::LAnd,
        TokKind::OrOr => Op::LOr,
        TokKind::Assign => Op::Assign,
        TokKind::PlusEq => Op::AddAssign,
        TokKind::MinusEq => Op::SubAssign,
        TokKind::StarEq => Op::MulAssign,
        TokKind::SlashEq => Op::DivAssign,
        TokKind::PercentEq => Op::ModAssign,
        TokKind::ShlEq => Op::ShlAssign,
        TokKind::ShrEq => Op::ShrAssign,
        TokKind::AmpEq => Op::AndAssign,
        TokKind::PipeEq => Op::OrAssign,
        TokKind::CaretEq => Op::XorAssign,
        TokKind::LParen => Op::Call,
        TokKind::LBracket => Op::Index,
        TokKind::Arrow => Op::Arrow,
        TokKind::Comma => Op::Comma,
        // 自增/自减与一元 `~` / `!`: lexer 会把它们合成单 token
        // (PlusPlus/MinusMinus/Tilde/Not), 之前缺映射导致
        // `C& operator++() { ... }` 在 parse_operator_name 里失败,
        // 进而整条成员声明 (含其后的数据成员) 退化成 Opaque.
        TokKind::PlusPlus => Op::Inc,
        TokKind::MinusMinus => Op::Dec,
        TokKind::Tilde => Op::Not,
        TokKind::Not => Op::LNot,
        TokKind::DotStar => Op::ArrowStar,
        _ => return None,
    })
}

/// 运算符重载函数修饰 (成员或非成员)
/// 格式: _ZN<class> op<args>E   或   _Z op<args>E
pub fn mangle_operator_member(class: &str, op: Operator, params: &[MangleType]) -> String {
    let n = NestedName::from_dotted(vec![class.to_string(), op.encode_name().to_string()]);
    let mut out = String::new();
    n.encode(&mut out);
    if params.is_empty() {
        out.push('v');
    } else {
        for p in params {
            p.encode(&mut out);
        }
    }
    out
}

pub fn mangle_operator_global(op: Operator, params: &[MangleType]) -> String {
    let mut out = String::new();
    out.push_str("_Z");
    out.push_str(op.encode_name());
    if params.is_empty() {
        out.push('v');
    } else {
        for p in params {
            p.encode(&mut out);
        }
    }
    out
}

/// vtable 符号: _ZTV<name>
pub fn mangle_vtable(name: &str) -> String {
    format!("_ZTV{}", encode_type_name(name))
}

/// typeinfo 符号: _ZTI<name>
pub fn mangle_typeinfo(name: &str) -> String {
    format!("_ZTI{}", encode_type_name(name))
}

/// 把源语言类型名编码成 Itanium `<unqualified-name>` / `<nested-name>`。
///
/// * `Foo` -> `3Foo`
/// * `Outer::Inner` -> `N5Outer5InnerE`
///
/// 模板实例名 (`__dcc_template_...`) 等非标准名按字面长度前缀处理，
/// 只保证符号唯一且合法，不与 libstdc++ 交换 RTTI。
pub fn encode_type_name(name: &str) -> String {
    let parts: Vec<&str> = name.split("::").collect();
    if parts.len() <= 1 {
        let seg = parts.first().copied().unwrap_or("");
        return format!("{}{}", seg.len(), seg);
    }
    let mut out = String::from("N");
    for p in parts {
        out.push_str(&format!("{}{}", p.len(), p));
    }
    out.push('E');
    out
}

/// operator new(size_t) Itanium 名字: _Znwm
pub fn mangle_op_new() -> &'static str {
    "_Znwm"
}
/// operator new[](size_t) Itanium 名字: _Znam
pub fn mangle_op_new_array() -> &'static str {
    "_Znam"
}
/// operator delete(void*) Itanium 名字: _ZdlPv
pub fn mangle_op_delete() -> &'static str {
    "_ZdlPv"
}
/// operator delete[](void*) Itanium 名字: _ZdaPv
pub fn mangle_op_delete_array() -> &'static str {
    "_ZdaPv"
}

/// 虚表项的类型信息符号: _ZTT<name> (抛掷类型表, gcc 用)
pub fn mangle_ttt(name: &str) -> String {
    format!("_ZTT{}{}", name.len(), name)
}

/// 模板实例化: 在 nested name 末尾追加类型参数
/// 例: std::vector<int>   ->   St6vectorIiSaIiEE (M8 简化)
pub fn mangle_template_args(prefix: &str, args: &[MangleType]) -> String {
    let mut out = String::new();
    out.push_str("_Z");
    out.push_str(&format!("{}{}", prefix.len(), prefix));
    for a in args {
        a.encode(&mut out);
    }
    out.push('E');
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
            mangle_member_function(
                &n,
                &[MangleType::Pointer(Box::new(MangleType::Int))],
                &MangleType::Void
            ),
            "_ZN3Foo3barEPi"
        );
    }
    // P2.4: operator new / new[] / delete / delete[] Itanium
    // 名称常量, lower 阶段以前硬编码 "_Znwm" / "_Znam" 等
    // 字符串, 现统一走 mangle.rs 命名常量以避免 typo.
    #[test]
    fn mangle_op_new_constants() {
        assert_eq!(mangle_op_new(), "_Znwm");
        assert_eq!(mangle_op_new_array(), "_Znam");
        assert_eq!(mangle_op_delete(), "_ZdlPv");
        assert_eq!(mangle_op_delete_array(), "_ZdaPv");
    }
    #[test]
    fn mangle_reference_type() {
        let n = NestedName::from_dotted(vec!["Foo".into(), "bar".into()]);
        assert_eq!(
            mangle_member_function(
                &n,
                &[MangleType::Reference(Box::new(MangleType::Int))],
                &MangleType::Void
            ),
            "_ZN3Foo3barERi"
        );
    }
    #[test]
    fn mangle_global() {
        assert_eq!(mangle_global_function("foo", &[MangleType::Int, MangleType::Int]), "_Z3fooii");
    }

    // M18.1: 模板实例化 T<U> 编码 — 与 St6vectorIiSaIiEE 一致
    #[test]
    fn encode_template_int() {
        let t = MangleType::Template {
            name: "vector".to_string(),
            args: vec![MangleType::Int],
        };
        let mut s = String::new();
        t.encode(&mut s);
        assert_eq!(s, "N6vectorIiE");
    }

    #[test]
    fn encode_template_multi_args() {
        let t = MangleType::Template {
            name: "map".to_string(),
            args: vec![MangleType::Class("string".to_string()), MangleType::Int],
        };
        let mut s = String::new();
        t.encode(&mut s);
        // MangleType::Class 自身编码为 N<len><name>E, 形参里再嵌套 N<...>E
        assert_eq!(s, "N3mapI6NstringEiE");
    }

    #[test]
    fn encode_template_pointer_arg() {
        let t = MangleType::Template {
            name: "X".to_string(),
            args: vec![MangleType::Pointer(Box::new(MangleType::Int))],
        };
        let mut s = String::new();
        t.encode(&mut s);
        assert_eq!(s, "N1XIPiE");
    }

    #[test]
    fn encode_nested_template() {
        // vector<pair<int, int>>
        let t = MangleType::Template {
            name: "vector".to_string(),
            args: vec![MangleType::Template {
                name: "pair".to_string(),
                args: vec![MangleType::Int, MangleType::Int],
            }],
        };
        let mut s = String::new();
        t.encode(&mut s);
        // pair<int,int> 编码为 N4pairIiiE (两 int 直接拼接)
        // vector<pair<int,int>> 编码为 N6vectorIN4pairIiiEE
        assert_eq!(s, "N6vectorIN4pairIiiEE");
    }

    /// 回归 (cpp98_iterator_traits 兼查出的前端缺陷): lexer 会把 `++` / `--` /
    /// `!` / `~` / `.*` 合成单 token, 而 `from_tok_kind` 缺这些映射时
    /// `parse_operator_name` 直接失败 -> 类成员循环把整条声明连同其后的数据
    /// 成员一起降级成 Opaque (`int* p_;` 就此消失, sizeof(类) 少 8 字节,
    /// 成员函数体里报 "undeclared identifier: p_").
    #[test]
    fn from_tok_kind_covers_multi_char_and_unary_ops() {
        assert_eq!(from_tok_kind(&TokKind::PlusPlus), Some(Operator::Inc));
        assert_eq!(from_tok_kind(&TokKind::MinusMinus), Some(Operator::Dec));
        assert_eq!(from_tok_kind(&TokKind::Tilde), Some(Operator::Not));
        assert_eq!(from_tok_kind(&TokKind::Not), Some(Operator::LNot));
        assert_eq!(from_tok_kind(&TokKind::DotStar), Some(Operator::ArrowStar));
        // 命中的运算符必须落入正确的 Itanium 编码槽
        assert_eq!(Operator::Inc.encode_name(), "pp");
        assert_eq!(Operator::Dec.encode_name(), "mm");
        assert_eq!(Operator::Not.encode_name(), "co");
        assert_eq!(Operator::LNot.encode_name(), "nt");
        assert_eq!(Operator::ArrowStar.encode_name(), "pm");
    }

    /// 字符拼接路径 (单 token 未被 lexer 合并时) 也要认 `!` / `~`.
    #[test]
    fn operator_from_token_unary_spellings() {
        assert_eq!(Operator::from_token("++"), Some(Operator::Inc));
        assert_eq!(Operator::from_token("--"), Some(Operator::Dec));
        assert_eq!(Operator::from_token("!"), Some(Operator::LNot));
        assert_eq!(Operator::from_token("~"), Some(Operator::Not));
        assert_eq!(mangle_operator_global(Operator::Inc, &[]), "_Zppv");
    }
}
