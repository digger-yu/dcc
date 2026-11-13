// libm 数学内建（`__builtin_<name>[f|l]`）的**单一名字表**。
//
// 数学内建在 dcc 里要跨两处处理：
//   1. parser 需要**返回类型**（缺了会走 W1015「assuming int」，浮点结果被当
//      整数搬运/截断，`tree-ssa/ldexp.c` 因此算出垃圾）；
//   2. codegen 需要知道**转发到哪个 libm 符号**（缺了报 E0018 硬错误，
//      `torture/builtin-math-2.c` 等 11 个用例编译失败）。
//
// 两处各手写一份清单必然漂移 —— 曾经的实况就是 `call_result_type` 有
// `__builtin_fmod` 而 `gen_builtin` 没有，一半按 int 取结果、一半报未实现。
// 所以名字表放在这里，parser 与 codegen 共用同一份判据。
//
// 后缀规则（与 GCC 的 builtin 命名一致）：
//   - 无后缀 → double 语义，libm 符号 `<base>`；
//   - `f`     → float，libm 符号 `<base>f`；
//   - `l`     → long double，libm 符号 `<base>l`。
//
// 剥离后缀前**必须**先确认 `<base>` 在表里，不能无条件剥末字符：
//   - `erf`/`erfc` 本身以 f 结尾，剥成 `er` 就不是内建了；
//   - `lrint`/`lround` 剥 l 会变成合法的 `rint`/`round`，返回类型从
//     long 错成 long double —— 所以整表必须先按**全名**查一遍。
//   - `lgamma` 剥 l 后剩 `gamma`（老式 libm 名，等价 lgamma），故意不放进表里，
//     否则 `__builtin_lgamma` 会被当成 long double 版。
//
// `cabs`/`carg`/`cimag`/`creal` 是「复数实参 → 实浮点返回」，与
// `csqrt`/`cpow` 这类「复数实参 → 复数返回」分开列，否则返回值宽度会错。

use crate::ast::Type;

/// 精度后缀：决定返回类型与 libm 符号尾缀。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MathSuffix {
    None,
    Float,
    LongDouble,
}

impl MathSuffix {
    /// 该精度对应的实浮点类型。
    pub fn real_type(self) -> Type {
        match self {
            MathSuffix::None => Type::Double,
            MathSuffix::Float => Type::Float,
            MathSuffix::LongDouble => Type::LongDouble,
        }
    }

    /// libm 符号尾缀：`<base>` + 本串。
    pub fn symbol_suffix(self) -> &'static str {
        match self {
            MathSuffix::None => "",
            MathSuffix::Float => "f",
            MathSuffix::LongDouble => "l",
        }
    }
}

/// 名字类别：决定返回类型与 codegen 的转发方式。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MathClass {
    /// 实参实数、返回同精度实数（acos/sqrt/ldexp/fmod/...）。
    Real,
    /// 实参复数、返回同精度**实数**（cabs/carg/cimag/creal）。
    ComplexToReal,
    /// 实参复数、返回同精度**复数**（csqrt/cpow/conj/cproj/...）。
    ComplexToComplex,
    /// 返回 int 的谓词（isnan/isgreater/signbit/...）。不转发 libm。
    Predicate,
    /// 返回整数但宽度由基名决定（ilogb→int, lrint→long, llrint→long long）。
    IntMath,
}

/// 一次成功分类的结果。
#[derive(Clone, Copy, Debug)]
pub struct MathBuiltin {
    /// 表中登记的基础名（无 f/l 后缀）。
    pub base: &'static str,
    /// 精度后缀。
    pub suffix: MathSuffix,
    /// 类别。
    pub class: MathClass,
    /// 重入版（`lgamma_r` 家族）：符号名以 `_r` 结尾，调用多一个 `int *` 出参。
    pub reentrant: bool,
}

/// 实数实参、实数返回的基名。
///
/// `pow`/`sqrt`/`fabs`/`fma`/`fmin`/`fmax`/`modf`/`copysign` 等在 codegen
/// 里已有专门实现（内联指令或特殊通道），登记在此只为让 **parser 的返回类型**
/// 走同一份判据；codegen 的显式分支在 match 中排在转发之前，不会被抢走。
const REAL_BASES: &[&str] = &[
    // 三角 / 反三角 / 双曲
    "acos", "acosh", "asin", "asinh", "atan", "atan2", "atanh", "cos", "cosh", "sin", "sinh",
    "tan", "tanh",
    // 指数 / 对数
    "exp", "exp2", "exp10", "expm1", "log", "log2", "log10", "log1p", "logb", "pow", "sqrt",
    "cbrt", "hypot",
    // 误差函数 / 伽马（`gamma` 与 `lgamma` 是 glibc 并存的两个老符号）
    "erf", "erfc", "lgamma", "gamma", "tgamma",
    // 取整（浮点返回）
    "ceil", "floor", "round", "trunc", "nearbyint", "rint",
    // 分解 / 组合 / 缩放（drem/scalb/pow10/significand 是 BSD/C99 遗留名，
    // glibc 仍导出；GCC 的 `__builtin_*` 也照旧提供）
    "frexp", "ldexp", "scalbn", "scalbln", "scalb", "remquo", "remainder", "drem", "fmod",
    "fdim", "fmax", "fmin", "fma", "nextafter", "nexttoward", "copysign", "modf", "fabs",
    "significand", "pow10",
    // Bessel
    "j0", "j1", "jn", "y0", "y1", "yn",
    // C23 的 π 族
    "sinpi", "cospi", "tanpi", "asinpi", "acospi", "atanpi", "atan2pi",
];

/// 复数实参、实浮点返回的基名（复数模/幅角/实部/虚部）。
const COMPLEX_TO_REAL_BASES: &[&str] = &["cabs", "carg", "cimag", "creal"];

/// 复数实参、复数返回的基名。
const COMPLEX_TO_COMPLEX_BASES: &[&str] = &[
    "cacos", "cacosh", "casin", "casinh", "catan", "catanh", "ccos", "ccosh", "cexp", "clog",
    "clog10", "conj", "cpow", "cproj", "csin", "csinh", "csqrt", "ctan", "ctanh",
];

/// 返回 int 的谓词基名。
const PREDICATE_BASES: &[&str] = &[
    "isnan",
    "isnormal",
    "isinf",
    "isinf_sign",
    "isfinite",
    // GCC 也给非 `is` 前缀的 `__builtin_finite` 家族提供了内建（返回 int）。
    "finite",
    "isunordered",
    "fpclassify",
    "isgreater",
    "isgreaterequal",
    "isless",
    "islessequal",
    "islessgreater",
    "iseqsig",
    "signbit",
    "issignaling",
    "issubnormal",
    "iscanonical",
    "iszero",
];

/// 整数返回的数学基名：`ilogb→int`、`lrint/lround→long`、`llrint/llround→long long`。
const INT_MATH_BASES: &[&str] = &["ilogb", "lrint", "lround", "llrint", "llround"];

/// 按**全名**查表（不剥后缀）。命中返回 (表内静态名, 类别)。
fn lookup_exact(name: &str) -> Option<(&'static str, MathClass)> {
    for &b in REAL_BASES {
        if b == name {
            return Some((b, MathClass::Real));
        }
    }
    for &b in COMPLEX_TO_REAL_BASES {
        if b == name {
            return Some((b, MathClass::ComplexToReal));
        }
    }
    for &b in COMPLEX_TO_COMPLEX_BASES {
        if b == name {
            return Some((b, MathClass::ComplexToComplex));
        }
    }
    for &b in PREDICATE_BASES {
        if b == name {
            return Some((b, MathClass::Predicate));
        }
    }
    for &b in INT_MATH_BASES {
        if b == name {
            return Some((b, MathClass::IntMath));
        }
    }
    None
}

/// 分类一个**不带** `__builtin_` 前缀的内建名。
///
/// 先按全名查（`lrint`/`lround` 因此不会被误剥成 long double 的 `rint`/`round`），
/// 未命中再剥一层 `f`/`l` 后缀重查。
pub fn classify(bare: &str) -> Option<MathBuiltin> {
    if let Some((base, class)) = lookup_exact(bare) {
        return Some(MathBuiltin {
            base,
            suffix: MathSuffix::None,
            class,
            reentrant: false,
        });
    }
    // 重入版：`lgamma_r` / `lgammaf_r` / `lgammal_r`（以及老名 `gamma_r` 家族）。
    // 后缀顺序是「精度 + _r」（`lgammaf_r`），所以先剥 `_r` 再递归复用精度后缀
    // 逻辑，剥出来的 `lgammaf` 才会被判成 Float。直接整名登记不行 —— 六种拼写
    // 要在三张表里各写一遍。
    if let Some(stem) = bare.strip_suffix("_r") {
        let mut b = classify(stem)?;
        b.reentrant = true;
        return Some(b);
    }
    // 只剥一层：`erff` 剥 f 得 `erf`（在表），`erf` 本身在表不会走到这里；
    // `llrintf` 剥 f 得 `llrint`（在表）✓。
    let (stem, suffix) = if let Some(s) = bare.strip_suffix('f') {
        (s, MathSuffix::Float)
    } else if let Some(s) = bare.strip_suffix('l') {
        (s, MathSuffix::LongDouble)
    } else {
        return None;
    };
    let (base, class) = lookup_exact(stem)?;
    Some(MathBuiltin {
        base,
        suffix,
        class,
        reentrant: false,
    })
}

/// `__builtin_xxx` 的返回类型；不是已知数学内建时返回 `None`。
pub fn result_type(prefixed: &str) -> Option<Type> {
    let b = classify(prefixed.strip_prefix("__builtin_")?)?;
    Some(match b.class {
        MathClass::Predicate => Type::Int,
        MathClass::IntMath => match b.base {
            "ilogb" => Type::Int,
            "lrint" | "lround" => Type::Long,
            _ => Type::LongLong,
        },
        // 返回实浮点：精度后缀决定宽度。
        MathClass::Real | MathClass::ComplexToReal => b.suffix.real_type(),
        // 返回复数：内型由精度后缀决定。
        MathClass::ComplexToComplex => Type::Complex(Box::new(b.suffix.real_type().into())),
    })
}

/// 该内建应转发到的 libm 符号（谓词/整数返回类不转发，返回 `None`）。
pub fn libm_symbol(prefixed: &str) -> Option<String> {
    let b = classify(prefixed.strip_prefix("__builtin_")?)?;
    match b.class {
        MathClass::Predicate | MathClass::IntMath => None,
        _ => Some(format!(
            "{}{}{}",
            b.base,
            b.suffix.symbol_suffix(),
            if b.reentrant { "_r" } else { "" }
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffix_traps() {
        // `erf` 自身以 f 结尾，不能被剥成 `er`。
        let e = classify("erf").unwrap();
        assert_eq!((e.base, e.suffix), ("erf", MathSuffix::None));
        let ef = classify("erff").unwrap();
        assert_eq!((ef.base, ef.suffix), ("erf", MathSuffix::Float));
        // `lrint` 剥 l 会撞上 `rint`（long double 版），必须整名优先。
        let lr = classify("lrint").unwrap();
        assert_eq!(lr.class, MathClass::IntMath);
        assert_eq!(lr.suffix, MathSuffix::None);
        assert_eq!(result_type("__builtin_lrintl"), Some(Type::Long));
        // `lgamma` 剥 l 剩 `gamma`，不在表里 —— 不能被误判。
        let lg = classify("lgamma").unwrap();
        assert_eq!((lg.base, lg.suffix), ("lgamma", MathSuffix::None));
        // `lgammaf` 是 float 版 lgamma，不是 `lgamma`+f 之外的怪东西。
        let lgf = classify("lgammaf").unwrap();
        assert_eq!((lgf.base, lgf.suffix), ("lgamma", MathSuffix::Float));
        // `fabs` 以 f 开头但不在结尾，`fabsf` 才是 float 版。
        let fa = classify("fabs").unwrap();
        assert_eq!((fa.base, fa.suffix), ("fabs", MathSuffix::None));
        let faf = classify("fabsf").unwrap();
        assert_eq!((faf.base, faf.suffix), ("fabs", MathSuffix::Float));
    }

    #[test]
    fn reentrant_lgamma() {
        // `_r` 后缀在精度后缀之后：lgammaf_r 是 float 版，不是 lgamma_r 的 f 变体。
        let r = classify("lgamma_r").unwrap();
        assert_eq!((r.base, r.suffix, r.reentrant), ("lgamma", MathSuffix::None, true));
        let rf = classify("lgammaf_r").unwrap();
        assert_eq!((rf.base, rf.suffix, rf.reentrant), ("lgamma", MathSuffix::Float, true));
        assert_eq!(libm_symbol("__builtin_lgammal_r").as_deref(), Some("lgammal_r"));
        assert_eq!(result_type("__builtin_gamma_r"), Some(Type::Double));
        assert_eq!(libm_symbol("__builtin_gammaf_r").as_deref(), Some("gammaf_r"));
    }

    #[test]
    fn complex_kinds() {
        assert_eq!(
            result_type("__builtin_cabs"),
            Some(Type::Double),
            "cabs 返回实浮点"
        );
        assert_eq!(
            result_type("__builtin_cabsf"),
            Some(Type::Float),
            "cabsf 返回 float"
        );
        assert!(matches!(
            result_type("__builtin_csqrtf"),
            Some(Type::Complex(_))
        ));
        assert_eq!(
            libm_symbol("__builtin_cabs").as_deref(),
            Some("cabs"),
            "cabs 要转发到 libm 的 cabs 符号"
        );
    }

    #[test]
    fn integer_math_widths() {
        assert_eq!(result_type("__builtin_ilogb"), Some(Type::Int));
        assert_eq!(result_type("__builtin_ilogbl"), Some(Type::Int));
        assert_eq!(result_type("__builtin_lroundf"), Some(Type::Long));
        assert_eq!(result_type("__builtin_llround"), Some(Type::LongLong));
        // 整数返回类不转发 libm（符号名不同，且回归会变成 int）。
        assert_eq!(libm_symbol("__builtin_lrint"), None);
    }

    #[test]
    fn predicates_are_not_forwarded() {
        assert_eq!(result_type("__builtin_isnanf"), Some(Type::Int));
        assert_eq!(libm_symbol("__builtin_isnan"), None);
        assert_eq!(result_type("__builtin_unknown_thing"), None);
    }
}
