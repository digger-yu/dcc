//! C++ 前端 "内置函数" (builtin) 声明的单一真源 (single source of truth).
//!
//! # 为什么需要这一层
//!
//! GCC/Clang 默认 (`-fbuiltin`) 把一批 C 标准库函数当作 **语言级内置符号**:
//! 即使源文件没有 `#include <string.h>` / `<stdlib.h>`, 也可以直接调用
//! `memcpy` / `memset` / `strlen` / `malloc` ...
//!
//! C 路径靠 "隐式函数声明" 蒙混过关 (C89 规则), 但 C++ 早已删除隐式声明,
//! 因此 dcc++ 的 stdlib 头 (如 `<ranges>`) 里直接写 `memcpy(...)` 时,
//! native pipeline 的 sema 会以 `undeclared identifier: memcpy` 硬失败。
//! 真实 libstdc++ 之所以能编过, 是因为 `<ranges>` 传递包含了 `<cstring>`,
//! 而 `<cstring>` 又依赖 GCC 的 builtin 声明。
//!
//! # 设计
//!
//! - 本模块只有一张 **数据表** [`BUILTIN_LIBC_FUNCS`] 与两个查询函数, 不做任何
//!   按夹具/按调用点的特判。
//! - 注入时机是 **惰性的**: 只有当普通名字查找彻底失败时才查这张表并把符号
//!   登记进 **全局作用域** (见 `Sema::declare_builtin_if_known`)。
//!   这样显式声明 (例如 `<cstring>` 里的 `extern "C"` 原型) 永远优先,
//!   不会与真实声明产生重复符号, 也不会平移已有符号的 SymbolId。
//! - 表里的签名与 C 标准 (以及 `src/cpp/stdlib/headers/cstring`、`cstdlib`)
//!   保持一致; 若二者不一致, 以本表 + 头文件共同遵循的 C 标准为准。

use crate::cpp::sema::CxxType;

/// 内置函数签名里用到的类型.
///
/// 只覆盖 memory/string/alloc 类 builtin 需要的类型集合, 保持与
/// `CxxType` 的一对一映射 (const 限定不参与 dcc 的类型区分)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinTy {
    Void,
    Bool,
    Char,
    SChar,
    UChar,
    Short,
    UShort,
    Int,
    UInt,
    Long,
    ULong,
    LongLong,
    ULongLong,
    Float,
    Double,
    /// `void *`
    VoidPtr,
    /// `char *` (含 `const char *`)
    CharPtr,
    /// `signed char *` (含 `const signed char *`)
    SCharPtr,
    /// `wchar_t *` (含 const)
    WCharPtr,
}

impl BuiltinTy {
    /// 降到 Sema 的 C++ 类型表示。
    pub fn to_cxx(self) -> CxxType {
        match self {
            BuiltinTy::Void => CxxType::Void,
            BuiltinTy::Bool => CxxType::Bool,
            BuiltinTy::Char => CxxType::Integer {
                bits: 8,
                signed: true,
            },
            BuiltinTy::SChar => CxxType::Integer {
                bits: 8,
                signed: true,
            },
            BuiltinTy::UChar => CxxType::Integer {
                bits: 8,
                signed: false,
            },
            BuiltinTy::Short => CxxType::Integer {
                bits: 16,
                signed: true,
            },
            BuiltinTy::UShort => CxxType::Integer {
                bits: 16,
                signed: false,
            },
            BuiltinTy::Int => CxxType::Integer {
                bits: 32,
                signed: true,
            },
            BuiltinTy::UInt => CxxType::Integer {
                bits: 32,
                signed: false,
            },
            BuiltinTy::Long => CxxType::Integer {
                bits: 64,
                signed: true,
            },
            BuiltinTy::ULong => CxxType::Integer {
                bits: 64,
                signed: false,
            },
            BuiltinTy::LongLong => CxxType::Integer {
                bits: 64,
                signed: true,
            },
            BuiltinTy::ULongLong => CxxType::Integer {
                bits: 64,
                signed: false,
            },
            BuiltinTy::Float => CxxType::Float { bits: 32 },
            BuiltinTy::Double => CxxType::Float { bits: 64 },
            BuiltinTy::VoidPtr => CxxType::Pointer(Box::new(CxxType::Void)),
            BuiltinTy::CharPtr => CxxType::Pointer(Box::new(CxxType::Integer {
                bits: 8,
                signed: true,
            })),
            BuiltinTy::SCharPtr => CxxType::Pointer(Box::new(CxxType::Integer {
                bits: 8,
                signed: true,
            })),
            BuiltinTy::WCharPtr => CxxType::Pointer(Box::new(CxxType::Integer {
                bits: 32,
                signed: true,
            })),
        }
    }
}

/// 一条内置函数声明 (签名)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinFunc {
    /// C 链接名 (同时作为 Sema 全局符号名)
    pub name: &'static str,
    /// 返回类型
    pub ret: BuiltinTy,
    /// 形参类型列表
    pub params: &'static [BuiltinTy],
}

use BuiltinTy::*;

/// C/C++ 内置库函数表 —— 与 GCC `-fbuiltin` 默认开启的那批 libc 符号对齐。
///
/// 分组: 内存操作 / 字符串操作 / 动态存储。
pub static BUILTIN_LIBC_FUNCS: &[BuiltinFunc] = &[
    // ===== 内存操作 (memcpy 族) =====
    BuiltinFunc {
        name: "memcpy",
        ret: VoidPtr,
        params: &[VoidPtr, VoidPtr, ULong],
    },
    BuiltinFunc {
        name: "memmove",
        ret: VoidPtr,
        params: &[VoidPtr, VoidPtr, ULong],
    },
    BuiltinFunc {
        name: "memset",
        ret: VoidPtr,
        params: &[VoidPtr, Int, ULong],
    },
    BuiltinFunc {
        name: "bzero",
        ret: Void,
        params: &[VoidPtr, ULong],
    },
    BuiltinFunc {
        name: "bcopy",
        ret: Void,
        params: &[VoidPtr, VoidPtr, ULong],
    },
    BuiltinFunc {
        name: "memcmp",
        ret: Int,
        params: &[VoidPtr, VoidPtr, ULong],
    },
    BuiltinFunc {
        name: "memchr",
        ret: VoidPtr,
        params: &[VoidPtr, Int, ULong],
    },
    BuiltinFunc {
        name: "memrchr",
        ret: VoidPtr,
        params: &[VoidPtr, Int, ULong],
    },
    BuiltinFunc {
        name: "rawmemchr",
        ret: VoidPtr,
        params: &[VoidPtr, Int],
    },
    // wchar 版本 (宽字符为 int32)
    BuiltinFunc {
        name: "wmemcpy",
        ret: WCharPtr,
        params: &[WCharPtr, WCharPtr, ULong],
    },
    BuiltinFunc {
        name: "wmemmove",
        ret: WCharPtr,
        params: &[WCharPtr, WCharPtr, ULong],
    },
    BuiltinFunc {
        name: "wmemset",
        ret: WCharPtr,
        params: &[WCharPtr, Int, ULong],
    },
    BuiltinFunc {
        name: "wmemcmp",
        ret: Int,
        params: &[WCharPtr, WCharPtr, ULong],
    },
    // ===== 字符串操作 (strlen 族) =====
    BuiltinFunc {
        name: "strlen",
        ret: ULong,
        params: &[CharPtr],
    },
    BuiltinFunc {
        name: "strnlen",
        ret: ULong,
        params: &[CharPtr, ULong],
    },
    BuiltinFunc {
        name: "strcpy",
        ret: CharPtr,
        params: &[CharPtr, CharPtr],
    },
    BuiltinFunc {
        name: "strncpy",
        ret: CharPtr,
        params: &[CharPtr, CharPtr, ULong],
    },
    BuiltinFunc {
        name: "stpcpy",
        ret: CharPtr,
        params: &[CharPtr, CharPtr],
    },
    BuiltinFunc {
        name: "stpncpy",
        ret: CharPtr,
        params: &[CharPtr, CharPtr, ULong],
    },
    BuiltinFunc {
        name: "strcat",
        ret: CharPtr,
        params: &[CharPtr, CharPtr],
    },
    BuiltinFunc {
        name: "strncat",
        ret: CharPtr,
        params: &[CharPtr, CharPtr, ULong],
    },
    BuiltinFunc {
        name: "strcmp",
        ret: Int,
        params: &[CharPtr, CharPtr],
    },
    BuiltinFunc {
        name: "strncmp",
        ret: Int,
        params: &[CharPtr, CharPtr, ULong],
    },
    BuiltinFunc {
        name: "strcoll",
        ret: Int,
        params: &[CharPtr, CharPtr],
    },
    BuiltinFunc {
        name: "strchr",
        ret: CharPtr,
        params: &[CharPtr, Int],
    },
    BuiltinFunc {
        name: "strrchr",
        ret: CharPtr,
        params: &[CharPtr, Int],
    },
    BuiltinFunc {
        name: "strchrnul",
        ret: CharPtr,
        params: &[CharPtr, Int],
    },
    BuiltinFunc {
        name: "strstr",
        ret: CharPtr,
        params: &[CharPtr, CharPtr],
    },
    BuiltinFunc {
        name: "strpbrk",
        ret: CharPtr,
        params: &[CharPtr, CharPtr],
    },
    BuiltinFunc {
        name: "strdup",
        ret: CharPtr,
        params: &[CharPtr],
    },
    BuiltinFunc {
        name: "strndup",
        ret: CharPtr,
        params: &[CharPtr, ULong],
    },
    // BSD 别名
    BuiltinFunc {
        name: "index",
        ret: CharPtr,
        params: &[CharPtr, Int],
    },
    BuiltinFunc {
        name: "rindex",
        ret: CharPtr,
        params: &[CharPtr, Int],
    },
    // ===== 动态存储 =====
    BuiltinFunc {
        name: "malloc",
        ret: VoidPtr,
        params: &[ULong],
    },
    BuiltinFunc {
        name: "calloc",
        ret: VoidPtr,
        params: &[ULong, ULong],
    },
    BuiltinFunc {
        name: "realloc",
        ret: VoidPtr,
        params: &[VoidPtr, ULong],
    },
    BuiltinFunc {
        name: "free",
        ret: Void,
        params: &[VoidPtr],
    },
    BuiltinFunc {
        name: "aligned_alloc",
        ret: VoidPtr,
        params: &[ULong, ULong],
    },
    // ===== 进程控制 =====
    BuiltinFunc {
        name: "abort",
        ret: Void,
        params: &[],
    },
    BuiltinFunc {
        name: "exit",
        ret: Void,
        params: &[Int],
    },
];

/// 按名字查内置函数签名。
pub fn lookup(name: &str) -> Option<&'static BuiltinFunc> {
    // 表很小且按字母无序, 线性扫描即可 (只在名字查找失败的路径上被调用)。
    BUILTIN_LIBC_FUNCS.iter().find(|b| b.name == name)
}

/// 该名字是否属于内置库函数。
pub fn is_builtin(name: &str) -> bool {
    lookup(name).is_some()
}

/// 把 [`BuiltinFunc`] 展成 `CxxType::Function`。
pub fn cxx_signature(b: &BuiltinFunc) -> CxxType {
    CxxType::Function {
        ret: Box::new(b.ret.to_cxx()),
        params: b.params.iter().map(|p| p.to_cxx()).collect(),
        is_const: false,
        is_volatile: false,
        ref_qual: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_unique_names() {
        let mut seen: Vec<&str> = Vec::new();
        for b in BUILTIN_LIBC_FUNCS {
            assert!(!seen.contains(&b.name), "duplicate builtin entry: {}", b.name);
            seen.push(b.name);
            assert!(!b.name.is_empty());
        }
    }

    #[test]
    fn memory_builtins_are_registered_with_libc_signatures() {
        let memcpy = lookup("memcpy").expect("memcpy builtin");
        assert_eq!(memcpy.ret, VoidPtr);
        assert_eq!(memcpy.params, &[VoidPtr, VoidPtr, ULong]);

        let memset = lookup("memset").expect("memset builtin");
        assert_eq!(memset.ret, VoidPtr);
        assert_eq!(memset.params, &[VoidPtr, Int, ULong]);

        let memmove = lookup("memmove").expect("memmove builtin");
        assert_eq!(memmove.params, &[VoidPtr, VoidPtr, ULong]);

        let strlen = lookup("strlen").expect("strlen builtin");
        assert_eq!(strlen.ret, ULong);
        assert_eq!(strlen.params, &[CharPtr]);
    }

    #[test]
    fn unknown_names_are_not_builtins() {
        assert!(!is_builtin("my_custom_function"));
        assert!(lookup("__builtin_memcpy").is_none());
        assert!(lookup("printf").is_none());
    }

    #[test]
    fn cxx_signature_round_trip() {
        let b = lookup("memcmp").unwrap();
        let ty = cxx_signature(b);
        match ty {
            CxxType::Function { ret, params, .. } => {
                assert_eq!(
                    *ret,
                    CxxType::Integer {
                        bits: 32,
                        signed: true
                    }
                );
                assert_eq!(params.len(), 3);
                assert_eq!(params[0], CxxType::Pointer(Box::new(CxxType::Void)));
                assert_eq!(
                    params[2],
                    CxxType::Integer {
                        bits: 64,
                        signed: false
                    }
                );
            },
            other => panic!("expected Function type, got {:?}", other),
        }
    }

    /// stdlib 头里的 `extern "C"` 原型必须与 builtin 表一致, 否则同一个
    /// 符号会有两套签名。这里对最常用的一批做静态守卫。
    #[test]
    fn builtin_table_agrees_with_cstring_header() {
        let header = include_str!("stdlib/headers/cstring");
        for name in ["memcpy", "memmove", "strcmp", "strlen", "memset", "memcmp"] {
            assert!(header.contains(name), "<cstring> 头缺少 builtin 同名声明: {}", name);
            assert!(is_builtin(name), "builtin 表缺少 {}", name);
        }
    }
}
