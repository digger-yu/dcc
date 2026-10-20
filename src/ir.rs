//! Parser AST 与机器码后端之间的 typed IR。
//!
//! 该层保留 AST 作为后端迁移期间的语义载荷，同时建立独立、可验证的
//! 类型化函数/表达式图。后端入口只接受通过验证的 `Module`。

use crate::ast::{self, BinOp, BlockItem, Decl, Expr, InitItem, QualifierFlags, Stmt, Type, UnOp};
use crate::const_eval;
use crate::diagnostics::{Diagnostic, Severity, Span};
use std::collections::HashMap;

// Re-export ast::Program so it can be accessed as crate::ir::Program
pub use ast::Program;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueType {
    Void,
    Integer {
        bits: u16,
        signed: bool,
    },
    Float {
        bits: u16,
    },
    /// IEEE binary128 (`__float128` / `_Float128`)：113 位尾数 + 15 位指数。
    ///
    /// 为什么不复用 `Float { bits: 128 }`：x86-64 上 `long double` 是 x87
    /// 扩展精度（80 位有效数，16 字节槽里只有前 10 字节有效），而 binary128 是
    /// 严格的 IEEE 128 位格式、走 xmm0 16 字节通道。两者位型与调用约定都不同，
    /// 塌缩成同一个 `bits: 128` 会让后端无法区分值通道，故单独成变体。
    Float128,
    Pointer(Box<ValueType>),
    Array(Box<ValueType>, Option<usize>),
    Aggregate {
        name: String,
        size: usize,
        is_union: bool,
    },
    Function {
        ret: Box<ValueType>,
        params: Vec<ValueType>,
    },
    Vector(Box<ValueType>, usize),
    Complex(Box<ValueType>),
    Imaginary(Box<ValueType>), // _Imaginary: 仅虚部, 大小同内型
}

/// 编译期可确定的标量字面量（供多架构 IR lowering 使用）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExprLiteral {
    Int(i128),
    Float(crate::ast::FloatConst),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExprKind {
    Constant,
    Local(String),
    Binary(BinOp),
    Unary(UnOp),
    Assign,
    Cast,
    Address,
    Dereference,
    Index,
    Member(String),
    Call,
    /// M25: `__builtin_va_arg(ap, T)` 在 aarch64/riscv64 后端
    /// 需特殊处理 (走寄存器保存区, 跟普通 Call 区别开).
    /// `ap` 保留 ap 变量名便于 codegen 查 var_map.
    /// `arg_ty` 保留要读取的类型信息, 便于 codegen 层类型分派.
    VaArg {
        ap: String,
        arg_ty: ValueType,
    },
    Conditional,
    Aggregate,
    SizeAlignOffset,
    ControlFlow,
    Opaque,
}

#[derive(Debug, Clone)]
pub struct TypedExpr {
    pub ty: ValueType,
    pub kind: ExprKind,
    pub children: Vec<TypedExpr>,
    pub span: Span,
    /// `ExprKind::Constant` 等节点的字面量值
    pub literal: Option<ExprLiteral>,
    /// Type qualifiers (const/volatile/restrict) from the AST level.
    /// Propagated from QualType nodes during lowering.
    pub quals: QualifierFlags,
}

/// Typed IR statement.
///
/// Statements are represented in a flat, expression-oriented form:
/// - `expressions` holds all top-level expressions in the statement (e.g.
///   conditions, return values, the expression in an expression-statement).
/// - `nested` holds all sub-statements (e.g. then/else branches, loop body).
///
/// This structure is intentionally "kind-less" at the IR level — the
/// semantic shape is recoverable from the expression tree (via
/// `ExprKind::ControlFlow` etc.) and nesting.  Backends that need a
/// traditional tagged statement AST can rebuild it during lowering.
#[derive(Debug, Clone)]
pub struct TypedStmt {
    pub expressions: Vec<TypedExpr>,
    pub nested: Vec<TypedStmt>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Function {
    pub name: String,
    pub return_type: ValueType,
    pub params: Vec<ValueType>,
    pub body: Vec<TypedStmt>,
    pub span: Span,
}

impl Function {
    /// Get the function type for this Function.
    pub fn unknown_ty(&self) -> ValueType {
        ValueType::Function {
            ret: Box::new(self.return_type.clone()),
            params: self.params.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Global {
    pub name: String,
    pub ty: ValueType,
    pub initializer: Option<TypedExpr>,
    pub span: Span,
    pub is_tls: bool,
    /// 是否是函数内 static local (而非文件级全局变量)
    pub is_static_local: bool,
    /// 所属函数名 (static local 时 Some, 文件级全局变量时 None)
    pub owner_function: Option<String>,
    /// 对齐要求 (字节)
    pub alignment: usize,
    /// section 属性 (如 ".data.unlikely")
    pub section: Option<String>,
}

/// Structured type information extracted from AST type definitions.
///
/// Optimization passes and LTO can use this to reason about aggregate
/// types without walking the raw AST.  Each variant carries enough
/// metadata for size/alignment queries and member lookups that the
/// IR-level `ValueType::Aggregate` alone cannot provide.
#[derive(Debug, Clone)]
pub enum TypeDefInfo {
    /// `struct S { ... }` — fields with offsets.
    Struct {
        name: String,
        fields: Vec<TypeFieldInfo>,
        size: usize,
        align: usize,
    },
    /// `union U { ... }` — all fields share offset 0.
    Union {
        name: String,
        fields: Vec<TypeFieldInfo>,
        size: usize,
        align: usize,
    },
    /// `enum E { A = 0, B = 1, ... }`
    Enum {
        name: String,
        variants: Vec<(String, i64)>,
    },
    /// `typedef OldName NewName;`
    Typedef { name: String, underlying: ValueType },
    /// `__asm__("...")` at file scope — preserved for codegen.
    GlobalAsm { asm: String },
}

/// A single field inside a struct / union `TypeDefInfo`.
#[derive(Debug, Clone)]
pub struct TypeFieldInfo {
    pub name: String,
    pub ty: ValueType,
    /// Byte offset from the start of the containing aggregate.
    pub offset: usize,
    /// Field size in bytes.
    pub size: usize,
}

#[derive(Debug, Clone)]
pub struct Module {
    pub functions: Vec<Function>,
    pub globals: Vec<Global>,
    /// Type definitions (StructDef, UnionDef, EnumDef, Typedef) and
    /// GlobalAsm declarations that do not produce IR but must be
    /// preserved for the backend (debug info, codegen, etc.).
    pub type_defs: Vec<ast::Decl>,
    /// Structured type information derived from `type_defs`.
    /// Optimization passes can query this without walking the AST.
    type_info_cache: Vec<TypeDefInfo>,
    source: ast::Program,
}

impl Module {
    pub(crate) fn source(&self) -> &ast::Program {
        &self.source
    }
    pub(crate) fn source_mut(&mut self) -> &mut ast::Program {
        &mut self.source
    }

    /// Return structured type definition information for all type
    /// definitions in this module.  The result is lazily computed on
    /// first call and cached until the module is mutated (e.g. by
    /// `resync`).
    pub fn type_info(&mut self) -> &[TypeDefInfo] {
        if self.type_info_cache.is_empty() && !self.type_defs.is_empty() {
            self.type_info_cache = build_type_info(&self.type_defs);
        }
        &self.type_info_cache
    }

    /// Look up a struct/union type by its tag name (e.g. `"struct foo"`).
    pub fn find_aggregate(&mut self, tag: &str) -> Option<&TypeDefInfo> {
        self.type_info();
        self.type_info_cache
            .iter()
            .find(|info| matches!(info, TypeDefInfo::Struct { name, .. } | TypeDefInfo::Union { name, .. } if name == tag))
    }

    /// Look up an enum type by its tag name (e.g. `"enum color"`).
    pub fn find_enum(&mut self, tag: &str) -> Option<&TypeDefInfo> {
        self.type_info();
        self.type_info_cache
            .iter()
            .find(|info| matches!(info, TypeDefInfo::Enum { name, .. } if name == tag))
    }

    /// Look up a typedef by its alias name.
    pub fn find_typedef(&mut self, alias: &str) -> Option<&TypeDefInfo> {
        self.type_info();
        self.type_info_cache
            .iter()
            .find(|info| matches!(info, TypeDefInfo::Typedef { name, .. } if name == alias))
    }

    /// Collect all `GlobalAsm` strings (file-scope `__asm__` directives).
    pub fn global_asm_strings(&mut self) -> Vec<&str> {
        self.type_info();
        self.type_info_cache
            .iter()
            .filter_map(|info| match info {
                TypeDefInfo::GlobalAsm { asm } => Some(asm.as_str()),
                _ => None,
            })
            .collect()
    }
}

/// Build structured type info from raw AST declarations.
fn build_type_info(decls: &[ast::Decl]) -> Vec<TypeDefInfo> {
    let mut out = Vec::new();
    for decl in decls {
        match decl {
            ast::Decl::StructDef(sd) => {
                let (fields, size, align) = layout_struct_fields(&sd.fields);
                out.push(TypeDefInfo::Struct {
                    name: format!("struct {}", sd.name.name),
                    fields,
                    size,
                    align,
                });
            },
            ast::Decl::UnionDef(sd) => {
                let (fields, size, align) = layout_union_fields(&sd.fields);
                out.push(TypeDefInfo::Union {
                    name: format!("union {}", sd.name.name),
                    fields,
                    size,
                    align,
                });
            },
            ast::Decl::EnumDef(ed) => {
                let variants: Vec<(String, i64)> = ed
                    .variants
                    .iter()
                    .map(|(id, val)| (id.name.clone(), val.unwrap_or(0)))
                    .collect();
                out.push(TypeDefInfo::Enum {
                    name: format!("enum {}", ed.name.name),
                    variants,
                });
            },
            ast::Decl::Typedef { name, ty, .. } => {
                // We cannot fully lower the type without a Lowering
                // context, but we store a best-effort ValueType so
                // that passes can at least see the alias.
                let underlying = best_effort_lower_type(ty);
                out.push(TypeDefInfo::Typedef {
                    name: name.name.clone(),
                    underlying,
                });
            },
            ast::Decl::GlobalAsm { asm_str, .. } => {
                out.push(TypeDefInfo::GlobalAsm {
                    asm: asm_str.clone(),
                });
            },
            _ => {},
        }
    }
    out
}

/// Compute field layout for a struct (sequential, with padding).
fn layout_struct_fields(fields: &[ast::Field]) -> (Vec<TypeFieldInfo>, usize, usize) {
    let mut out = Vec::new();
    let mut offset: usize = 0;
    let mut max_align: usize = 1;
    for f in fields {
        let (sz, al) = type_size_align(&f.ty);
        if al > 1 {
            // Align to field alignment
            offset = (offset + al - 1) & !(al - 1);
        }
        out.push(TypeFieldInfo {
            name: f.name.clone(),
            ty: best_effort_lower_type(&f.ty),
            offset,
            size: sz,
        });
        offset += sz;
        if al > max_align {
            max_align = al;
        }
    }
    // Pad struct size to alignment
    if max_align > 1 {
        offset = (offset + max_align - 1) & !(max_align - 1);
    }
    (out, offset.max(1), max_align)
}

/// Compute field layout for a union (all fields at offset 0).
fn layout_union_fields(fields: &[ast::Field]) -> (Vec<TypeFieldInfo>, usize, usize) {
    let mut out = Vec::new();
    let mut max_size: usize = 0;
    let mut max_align: usize = 1;
    for f in fields {
        let (sz, al) = type_size_align(&f.ty);
        out.push(TypeFieldInfo {
            name: f.name.clone(),
            ty: best_effort_lower_type(&f.ty),
            offset: 0,
            size: sz,
        });
        if sz > max_size {
            max_size = sz;
        }
        if al > max_align {
            max_align = al;
        }
    }
    // Pad union size to alignment
    if max_align > 1 {
        max_size = (max_size + max_align - 1) & !(max_align - 1);
    }
    (out, max_size.max(1), max_align)
}

/// Best-effort size/alignment for an AST type (without full context).
fn type_size_align(ty: &ast::Type) -> (usize, usize) {
    let sz = ty.size();
    let al = sz.clamp(1, 16);
    (sz.max(1), al)
}

/// Best-effort AST→IR type lowering for type_info only.
/// This does NOT have access to the full type_map, so Named types
/// remain as opaque Aggregate placeholders.
fn best_effort_lower_type(ty: &ast::Type) -> ValueType {
    match ty {
        ast::Type::Void => ValueType::Void,
        ast::Type::Bool => ValueType::Integer {
            bits: 8,
            signed: false,
        },
        ast::Type::Char | ast::Type::SChar => ValueType::Integer {
            bits: 8,
            signed: true,
        },
        ast::Type::UChar | ast::Type::Char8 => ValueType::Integer {
            bits: 8,
            signed: false,
        },
        ast::Type::Char16 => ValueType::Integer {
            bits: 16,
            signed: false,
        },
        ast::Type::Char32 => ValueType::Integer {
            bits: 32,
            signed: false,
        },
        ast::Type::Short => ValueType::Integer {
            bits: 16,
            signed: true,
        },
        ast::Type::UShort => ValueType::Integer {
            bits: 16,
            signed: false,
        },
        ast::Type::Int => ValueType::Integer {
            bits: 32,
            signed: true,
        },
        ast::Type::UInt => ValueType::Integer {
            bits: 32,
            signed: false,
        },
        ast::Type::Long => ValueType::Integer {
            bits: 64,
            signed: true,
        },
        ast::Type::ULong => ValueType::Integer {
            bits: 64,
            signed: false,
        },
        ast::Type::LongLong => ValueType::Integer {
            bits: 64,
            signed: true,
        },
        ast::Type::ULongLong => ValueType::Integer {
            bits: 64,
            signed: false,
        },
        ast::Type::Int128 => ValueType::Integer {
            bits: 128,
            signed: true,
        },
        ast::Type::UInt128 => ValueType::Integer {
            bits: 128,
            signed: false,
        },
        ast::Type::Float => ValueType::Float { bits: 32 },
        ast::Type::Double => ValueType::Float { bits: 64 },
        ast::Type::LongDouble => ValueType::Float { bits: 128 },
        ast::Type::Float128 => ValueType::Float128,
        ast::Type::Fract => ValueType::Integer {
            bits: 32,
            signed: true,
        },
        ast::Type::Accum => ValueType::Integer {
            bits: 64,
            signed: true,
        },
        ast::Type::Ptr(inner) => ValueType::Pointer(Box::new(best_effort_lower_type(&inner.base))),
        ast::Type::Array(inner, sz) => {
            let elem = best_effort_lower_type(&inner.base);
            let n = match sz {
                ast::ArraySize::Const(n) => Some(*n),
                _ => None,
            };
            ValueType::Array(Box::new(elem), n)
        },
        ast::Type::Vector(inner, count) => {
            let elem = best_effort_lower_type(&inner.base);
            ValueType::Vector(Box::new(elem), *count)
        },
        ast::Type::Struct(name, _, _, _) => ValueType::Aggregate {
            name: name.clone(),
            size: ty.size().max(1),
            is_union: false,
        },
        ast::Type::Union(name, _, _, _) => ValueType::Aggregate {
            name: name.clone(),
            size: ty.size().max(1),
            is_union: true,
        },
        ast::Type::Enum(_, _, _) => ValueType::Integer {
            bits: 32,
            signed: true,
        },
        ast::Type::Func(ret, params) => ValueType::Function {
            ret: Box::new(best_effort_lower_type(&ret.base)),
            params: params
                .iter()
                .map(|p| best_effort_lower_type(&p.base))
                .collect(),
        },
        ast::Type::Complex(inner) => {
            ValueType::Complex(Box::new(best_effort_lower_type(&inner.base)))
        },
        ast::Type::Imaginary(inner) => {
            ValueType::Imaginary(Box::new(best_effort_lower_type(&inner.base)))
        },
        ast::Type::Atomic(inner) => best_effort_lower_type(&inner.base),
        ast::Type::Named(name) => {
            // Cannot resolve without type_map; use opaque aggregate
            ValueType::Aggregate {
                name: name.clone(),
                size: ty.size().max(1),
                is_union: false,
            }
        },
    }
}

/// Lower AST to IR.
///
/// Returns `Ok((module, warnings))` on success; warnings may be non-empty
/// (e.g. implicit-function-declaration fallback).
/// Returns `Err(errors)` only when there are actual `Severity::Error`
/// diagnostics that prevent further compilation.
pub fn lower(source: ast::Program) -> Result<(Module, Vec<Diagnostic>), Vec<Diagnostic>> {
    // Pre-scan type definitions for Type::Named resolution
    let mut type_map: HashMap<String, Type> = HashMap::new();
    for decl in &source.decls {
        match decl {
            Decl::StructDef(sd) => {
                let key = format!("struct {}", sd.name.name);
                type_map.entry(key).or_insert_with(|| {
                    Type::Struct(
                        format!("struct {}", sd.name.name),
                        std::sync::Arc::new(sd.fields.clone()),
                        1,
                        false,
                    )
                });
            },
            Decl::UnionDef(sd) => {
                let key = format!("union {}", sd.name.name);
                type_map.entry(key).or_insert_with(|| {
                    Type::Union(
                        format!("union {}", sd.name.name),
                        std::sync::Arc::new(sd.fields.clone()),
                        1,
                        false,
                    )
                });
            },
            Decl::EnumDef(ed) => {
                let key = format!("enum {}", ed.name.name);
                let variants: Vec<(String, i64)> = ed
                    .variants
                    .iter()
                    .map(|(id, val)| (id.name.clone(), val.unwrap_or(0)))
                    .collect();
                type_map.entry(key).or_insert_with(|| {
                    Type::Enum(
                        format!("enum {}", ed.name.name),
                        std::sync::Arc::new(variants),
                        None,
                    )
                });
            },
            Decl::Typedef { name, ty, .. } => {
                type_map
                    .entry(name.name.clone())
                    .or_insert_with(|| ty.clone());
            },
            _ => {},
        }
    }
    // 预分析 static local 变量, 用于 IR 层名字解析
    let sl_map = crate::static_locals::analyze_static_locals(&source);
    let mut cx = Lowering {
        diagnostics: Vec::new(),
        type_defs: type_map,
        sl_map,
        current_func: None,
    };
    let mut functions = Vec::new();
    let mut globals = Vec::new();
    let mut type_defs = Vec::new();
    for decl in &source.decls {
        match decl {
            Decl::Func(f) => {
                cx.current_func = Some(f.name.name.clone());
                let body = f.body.as_ref().map_or_else(Vec::new, |b| {
                    b.items
                        .iter()
                        .filter_map(|item| cx.lower_item(item))
                        .collect()
                });
                functions.push(Function {
                    name: f.name.name.clone(),
                    return_type: cx.lower_type(&f.ret_ty, &f.span),
                    params: f
                        .params
                        .iter()
                        .map(|p| cx.lower_type(&p.ty, &p.span))
                        .collect(),
                    body,
                    span: f.span.clone(),
                });
            },
            Decl::Var(v) => {
                if let Some(init) = &v.init {
                    if !is_static_initializer(init) {
                        cx.diagnostics.push(Diagnostic::err(
                            "E1006",
                            format!("global initializer for '{}' is not a supported constant expression", v.name.name),
                            v.span.clone(),
                        ));
                    }
                }
                globals.push(Global {
                    name: v.name.name.clone(),
                    ty: cx.lower_type(&v.ty, &v.span),
                    initializer: v.init.as_ref().map(|e| cx.lower_expr(e)),
                    span: v.span.clone(),
                    is_tls: v.is_thread_local,
                    is_static_local: false,
                    owner_function: None,
                    alignment: v.alignment.unwrap_or(v.ty.align()).max(1),
                    section: v.section.clone(),
                });
            },
            // Type definitions and global asm do not produce IR nodes
            // but are preserved in `type_defs` for the backend.
            Decl::StructDef(_) | Decl::UnionDef(_) | Decl::EnumDef(_) | Decl::Typedef { .. } => {
                type_defs.push(decl.clone());
            },
            Decl::GlobalAsm { .. } => {
                // Global asm has side effects (emitted into the output);
                // preserved in type_defs for codegen to process from AST.
                type_defs.push(decl.clone());
            },
        }
    }
    // static local 变量已在上方预分析并添加到 globals
    // (sl_map 在 Lowering 初始化时创建, 函数体降低时已用于名字解析)
    let sl_infos: Vec<_> = cx.sl_map.iter_all().cloned().collect();
    for info in sl_infos {
        globals.push(Global {
            name: info.mangled_name.clone(),
            ty: cx.lower_type(&info.ty, &Span::point("", 0, 0)),
            initializer: info.init.as_ref().map(|e| cx.lower_expr(e)),
            span: Span::point("", 0, 0),
            is_tls: false,
            is_static_local: true,
            owner_function: Some(info.owner_func.clone()),
            alignment: info.alignment,
            section: None,
        });
    }
    // Separate errors from warnings: only errors are fatal.
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    for d in cx.diagnostics {
        if matches!(d.sev, Severity::Error) {
            errors.push(d);
        } else {
            warnings.push(d);
        }
    }
    if errors.is_empty() {
        Ok((
            Module {
                functions,
                globals,
                type_defs,
                type_info_cache: Vec::new(),
                source,
            },
            warnings,
        ))
    } else {
        // Include warnings alongside fatal errors so the user sees them.
        errors.extend(warnings);
        Err(errors)
    }
}

pub fn validate(module: &Module) -> Result<(), Vec<Diagnostic>> {
    let mut diagnostics = Vec::new();
    for function in &module.functions {
        if function.name.is_empty() {
            diagnostics.push(Diagnostic::err(
                "E1002",
                "IR function has no name",
                function.span.clone(),
            ));
        }
        for stmt in &function.body {
            validate_stmt(stmt, &mut diagnostics);
        }
    }
    for global in &module.globals {
        if let Some(init) = &global.initializer {
            validate_expr(init, &mut diagnostics);
        }
        // Storage class 约束验证: static local 必须有所属函数和 mangled 名
        if global.is_static_local {
            if global.owner_function.is_none() {
                diagnostics.push(Diagnostic::err(
                    "E1090",
                    format!("static local '{}' has no owner function", global.name),
                    global.span.clone(),
                ));
            }
            if !global.name.starts_with(".Lsl") {
                diagnostics.push(Diagnostic::err(
                    "E1091",
                    format!(
                        "static local '{}' has non-mangled name (expected .Lsl prefix)",
                        global.name
                    ),
                    global.span.clone(),
                ));
            }
        }
    }
    if diagnostics.is_empty() {
        Ok(())
    } else {
        Err(diagnostics)
    }
}

/// opt 改写 AST 后，从 `Module::source` 重新 lower 函数/全局 IR 图。
pub fn resync(module: &mut Module) -> Result<(), Vec<Diagnostic>> {
    let (fresh, _warnings) = lower(module.source().clone())?;
    module.functions = fresh.functions;
    module.globals = fresh.globals;
    module.type_defs = fresh.type_defs;
    module.type_info_cache = Vec::new(); // invalidate cache
    Ok(())
}

fn validate_stmt(stmt: &TypedStmt, diagnostics: &mut Vec<Diagnostic>) {
    for expr in &stmt.expressions {
        validate_expr(expr, diagnostics);
    }
    for nested in &stmt.nested {
        validate_stmt(nested, diagnostics);
    }
}

/// 与 codegen 浮点路径一致的二元运算符集合。
const FLOAT_BINOPS: &[BinOp] = &[
    BinOp::Add,
    BinOp::Sub,
    BinOp::Mul,
    BinOp::Div,
    BinOp::Mod,
    BinOp::EqEq,
    BinOp::Ne,
    BinOp::Lt,
    BinOp::Le,
    BinOp::Gt,
    BinOp::Ge,
    BinOp::AndAnd,
    BinOp::OrOr,
];

/// 与 codegen 浮点路径一致的一元运算符集合。
const FLOAT_UNOPS: &[UnOp] = &[UnOp::Neg, UnOp::Plus, UnOp::Not];

/// 与 codegen SIMD 向量路径一致的二元运算符集合。
const VECTOR_BINOPS: &[BinOp] = &[
    BinOp::Add,
    BinOp::Sub,
    BinOp::Mul,
    BinOp::Div,
    BinOp::Mod,
    BinOp::And,
    BinOp::Or,
    BinOp::Xor,
    BinOp::Shl,
    BinOp::Shr,
    BinOp::EqEq,
    BinOp::Ne,
    BinOp::Lt,
    BinOp::Le,
    BinOp::Gt,
    BinOp::Ge,
];

/// 当前没有 codegen 支持的向量一元运算符。
const VECTOR_UNOPS: &[UnOp] = &[UnOp::Neg, UnOp::Plus, UnOp::BitNot, UnOp::Not];

/// 与 codegen 复数路径一致的二元运算符集合。
const COMPLEX_BINOPS: &[BinOp] = &[
    BinOp::Add,
    BinOp::Sub,
    BinOp::Mul,
    BinOp::Div,
    BinOp::EqEq,
    BinOp::Ne,
];

/// 与 codegen 复数路径一致的一元运算符集合。
const COMPLEX_UNOPS: &[UnOp] = &[UnOp::Neg, UnOp::Plus, UnOp::BitNot, UnOp::Not];

fn is_float_ty(ty: &ValueType) -> bool {
    matches!(ty, ValueType::Float { .. } | ValueType::Float128)
}

fn is_vector_ty(ty: &ValueType) -> bool {
    matches!(ty, ValueType::Vector(_, _))
}

fn is_complex_ty(ty: &ValueType) -> bool {
    matches!(ty, ValueType::Complex(_))
}

fn is_float_vector_ty(ty: &ValueType) -> bool {
    matches!(ty, ValueType::Vector(inner, _) if matches!(inner.as_ref(), ValueType::Float { .. }))
}

fn is_supported_vector_binop(op: BinOp, operands: &[TypedExpr]) -> bool {
    if !VECTOR_BINOPS.contains(&op) {
        return false;
    }
    if operands.iter().any(|e| is_float_vector_ty(&e.ty))
        && matches!(op, BinOp::Mod | BinOp::And | BinOp::Or | BinOp::Xor | BinOp::Shl | BinOp::Shr)
    {
        return false;
    }
    true
}

fn is_supported_float_cast(from: &ValueType, to: &ValueType) -> bool {
    if from == to || matches!(to, ValueType::Void) {
        return true;
    }
    // 浮点到复数的隐式转换
    if is_float_ty(from) && is_complex_ty(to) {
        return true;
    }
    // 复数到浮点的转换 (取实部)
    if is_complex_ty(from) && is_float_ty(to) {
        return true;
    }
    // binary128 与 float/double/整数之间的转换全部由 libgcc TFmode 承担，
    // 与 float/double 的那一族同权（`is_float_ty` 已含 `Float128`）。
    matches!(
        (from, to),
        (ValueType::Float { .. } | ValueType::Float128,
         ValueType::Float { .. } | ValueType::Float128)
            | (ValueType::Float { .. } | ValueType::Float128, ValueType::Integer { .. })
            | (ValueType::Integer { .. }, ValueType::Float { .. } | ValueType::Float128)
    )
}

pub fn value_type_size(ty: &ValueType) -> usize {
    match ty {
        ValueType::Void => 0,
        ValueType::Integer { bits, .. } | ValueType::Float { bits } => (*bits as usize) / 8,
        ValueType::Float128 => 16,
        ValueType::Pointer(_) | ValueType::Function { .. } => 8,
        ValueType::Array(inner, Some(n)) | ValueType::Vector(inner, n) => {
            value_type_size(inner) * *n
        },
        ValueType::Array(_, None) => 8,
        ValueType::Aggregate { size, .. } => *size,
        ValueType::Complex(inner) => value_type_size(inner) * 2,
        ValueType::Imaginary(inner) => value_type_size(inner),
    }
}

fn same_size_bitcast(from: &ValueType, to: &ValueType) -> bool {
    value_type_size(from) == value_type_size(to)
}

fn is_supported_vector_scalar_bitcast_ty(ty: &ValueType) -> bool {
    match ty {
        ValueType::Integer { bits, .. } => matches!(*bits, 8 | 16 | 32 | 64 | 128),
        ValueType::Float { bits } => matches!(*bits, 32 | 64),
        _ => false,
    }
}

fn is_supported_complex_integer_cast(ty: &ValueType) -> bool {
    matches!(
        ty,
        ValueType::Integer {
            bits: 8 | 16 | 32 | 64,
            signed: true
        }
    )
}

fn is_supported_vector_cast(from: &ValueType, to: &ValueType, src_kind: &ExprKind) -> bool {
    if from == to || matches!(to, ValueType::Void) {
        return true;
    }
    if matches!(to, ValueType::Vector(_, _)) && matches!(src_kind, ExprKind::Aggregate) {
        return true;
    }
    // 标量到向量的广播转换: (vector_type) scalar_value
    if matches!(to, ValueType::Vector(_, _)) && !is_vector_ty(from) {
        return matches!(from, ValueType::Integer { .. } | ValueType::Float { .. });
    }
    if !same_size_bitcast(from, to) {
        return false;
    }
    match (from, to) {
        (ValueType::Vector(_, _), ValueType::Vector(_, _)) => true,
        (ValueType::Vector(_, _), ValueType::Complex(_))
        | (ValueType::Complex(_), ValueType::Vector(_, _)) => true,
        (ValueType::Vector(_, _), other) | (other, ValueType::Vector(_, _)) => {
            is_supported_vector_scalar_bitcast_ty(other)
        },
        _ => false,
    }
}

fn is_supported_complex_cast(from: &ValueType, to: &ValueType) -> bool {
    if from == to || matches!(to, ValueType::Void) {
        return true;
    }
    // 复数到复数的转换: _Complex float -> _Complex double 等
    if is_complex_ty(from) && is_complex_ty(to) {
        return true;
    }
    if is_complex_ty(from) && is_supported_complex_integer_cast(to) {
        return true;
    }
    // 标量到复数的隐式转换
    if !is_complex_ty(from) && is_complex_ty(to) {
        return matches!(from, ValueType::Integer { .. } | ValueType::Float { .. });
    }
    same_size_bitcast(from, to)
        && matches!(
            (from, to),
            (ValueType::Complex(_), ValueType::Vector(_, _))
                | (ValueType::Vector(_, _), ValueType::Complex(_))
        )
}

fn is_va_list_struct(ty: &ValueType) -> bool {
    matches!(ty, ValueType::Aggregate { name, .. } if name == "__dcc_sysv_va_list")
}

fn is_va_list_type(ty: &ValueType) -> bool {
    match ty {
        ValueType::Array(inner, Some(1)) => is_va_list_struct(inner),
        ValueType::Pointer(inner) => {
            is_va_list_struct(inner)
                || matches!(inner.as_ref(), ValueType::Array(arr, Some(1)) if is_va_list_struct(arr))
        },
        _ => false,
    }
}

fn validate_expr(expr: &TypedExpr, diagnostics: &mut Vec<Diagnostic>) {
    match expr.kind {
        ExprKind::Binary(op) => {
            let float = expr.children.iter().any(|e| is_float_ty(&e.ty));
            let vector = expr.children.iter().any(|e| is_vector_ty(&e.ty));
            let complex = expr.children.iter().any(|e| is_complex_ty(&e.ty));
            if float && !FLOAT_BINOPS.contains(&op) {
                diagnostics.push(Diagnostic::err(
                    "E1003",
                    format!("unsupported floating-point operation {:?}", op),
                    expr.span.clone(),
                ));
            }
            if vector && !is_supported_vector_binop(op, &expr.children) {
                diagnostics.push(Diagnostic::err(
                    "E1007",
                    format!("unsupported vector operation {:?}", op),
                    expr.span.clone(),
                ));
            }
            if complex && !COMPLEX_BINOPS.contains(&op) {
                diagnostics.push(Diagnostic::err(
                    "E1008",
                    format!("unsupported complex operation {:?}", op),
                    expr.span.clone(),
                ));
            }
        },
        ExprKind::Unary(op) => {
            if let Some(operand) = expr.children.first() {
                if is_float_ty(&operand.ty) && !FLOAT_UNOPS.contains(&op) {
                    diagnostics.push(Diagnostic::err(
                        "E1003",
                        format!("unsupported floating-point unary operation {:?}", op),
                        expr.span.clone(),
                    ));
                }
                if is_vector_ty(&operand.ty) && !VECTOR_UNOPS.contains(&op) {
                    diagnostics.push(Diagnostic::err(
                        "E1007",
                        format!("unsupported vector unary operation {:?}", op),
                        expr.span.clone(),
                    ));
                }
                if is_complex_ty(&operand.ty) && !COMPLEX_UNOPS.contains(&op) {
                    diagnostics.push(Diagnostic::err(
                        "E1008",
                        format!("unsupported complex unary operation {:?}", op),
                        expr.span.clone(),
                    ));
                }
            }
        },
        ExprKind::Assign if expr.children.len() >= 2 => {
            let lhs = &expr.children[0];
            let rhs = &expr.children[1];
            if is_vector_ty(&lhs.ty)
                && !is_vector_ty(&rhs.ty)
                && !matches!(
                    rhs.kind,
                    ExprKind::Aggregate
                        | ExprKind::Cast
                        | ExprKind::Binary(_)
                        | ExprKind::Unary(_)
                        | ExprKind::Call
                )
            {
                diagnostics.push(Diagnostic::err(
                    "E1007",
                    "vector assignment requires a vector initializer or compatible cast",
                    expr.span.clone(),
                ));
            }
            // 复数赋值: 允许 complex = complex, complex = scalar (隐式转换)
            // 只禁止 complex = vector 等不兼容类型
            if is_complex_ty(&lhs.ty) && !is_complex_ty(&rhs.ty) && is_vector_ty(&rhs.ty) {
                diagnostics.push(Diagnostic::err(
                    "E1008",
                    "cannot assign vector to complex type",
                    expr.span.clone(),
                ));
            }
        },
        ExprKind::Conditional if expr.children.len() >= 3 => {
            let then_e = &expr.children[1];
            let else_e = &expr.children[2];
            if is_vector_ty(&expr.ty) && (!is_vector_ty(&then_e.ty) || !is_vector_ty(&else_e.ty)) {
                diagnostics.push(Diagnostic::err(
                    "E1007",
                    "vector conditional requires vector branches",
                    expr.span.clone(),
                ));
            }
            if is_complex_ty(&expr.ty) && (!is_complex_ty(&then_e.ty) || !is_complex_ty(&else_e.ty))
            {
                diagnostics.push(Diagnostic::err(
                    "E1008",
                    "complex conditional requires complex branches",
                    expr.span.clone(),
                ));
            }
        },
        ExprKind::Cast => {
            if let Some(src) = expr.children.first() {
                let is_valid_vector_cast = is_supported_vector_cast(&src.ty, &expr.ty, &src.kind);
                let is_valid_complex_cast = is_supported_complex_cast(&src.ty, &expr.ty);
                if (is_float_ty(&src.ty) || is_float_ty(&expr.ty))
                    && !is_supported_float_cast(&src.ty, &expr.ty)
                {
                    diagnostics.push(Diagnostic::err(
                        "E1003",
                        "unsupported cast involving floating-point type",
                        expr.span.clone(),
                    ));
                }
                let cast_supported = is_valid_vector_cast || is_valid_complex_cast;
                if (is_vector_ty(&src.ty) || is_vector_ty(&expr.ty)) && !cast_supported {
                    diagnostics.push(Diagnostic::err(
                        "E1007",
                        "unsupported cast involving vector type",
                        expr.span.clone(),
                    ));
                }
                if (is_complex_ty(&src.ty) || is_complex_ty(&expr.ty)) && !cast_supported {
                    diagnostics.push(Diagnostic::err(
                        "E1008",
                        "unsupported cast involving complex type",
                        expr.span.clone(),
                    ));
                }
            }
        },
        ExprKind::Aggregate => {
            if let ValueType::Vector(_, max) = &expr.ty {
                if expr.children.len() > *max {
                    diagnostics.push(Diagnostic::err(
                        "E1007",
                        format!(
                            "vector initializer has {} elements but type holds {}",
                            expr.children.len(),
                            max
                        ),
                        expr.span.clone(),
                    ));
                }
            }
        },
        ExprKind::VaArg { .. }
            if !expr
                .children
                .first()
                .is_some_and(|ap| is_va_list_type(&ap.ty)) =>
        {
            diagnostics.push(Diagnostic::err(
                "E1005",
                "va_arg currently requires a va_list object",
                expr.span.clone(),
            ));
        },
        _ => {},
    }
    for child in &expr.children {
        validate_expr(child, diagnostics);
    }
}

struct Lowering {
    diagnostics: Vec<Diagnostic>,
    /// Pre-scanned type definition map: name → resolved Type.
    /// Covers typedefs, struct/union/enum tags for Type::Named resolution.
    type_defs: HashMap<String, Type>,
    /// Static local 变量映射 (源名 -> mangled 名), 用于 Ident 解析。
    sl_map: crate::static_locals::StaticLocalMap,
    /// 当前正在降低的函数名 (用于 static local 解析)。
    current_func: Option<String>,
}

// ============================================================================
// IMPLEMENTATIONS FOR VALUE TYPES
// ============================================================================

impl ValueType {
    /// Compute the size in bytes of this type.
    pub fn size(&self) -> usize {
        match self {
            Self::Void => 0,
            Self::Integer { bits, .. } => (*bits as usize).div_ceil(8),
            Self::Float { bits } => (*bits as usize).div_ceil(8),
            Self::Float128 => 16,
            Self::Pointer(_pointee) => {
                // On all supported targets, pointers are 8 bytes (64-bit)
                std::mem::size_of::<usize>()
            },
            Self::Array(elem, count) => elem.size().saturating_mul(count.unwrap_or(0)),
            Self::Aggregate { size, .. } => *size,
            Self::Function { .. } => std::mem::size_of::<usize>(), // Function pointer
            Self::Vector(elem, len) => elem.size().saturating_mul(*len),
            Self::Complex(inner) => inner.size().saturating_mul(2),
            Self::Imaginary(inner) => inner.size(),
        }
    }

    /// Get alignment requirement in bytes.
    pub fn align(&self) -> usize {
        match self {
            Self::Void => 1,
            Self::Integer { bits, .. } | Self::Float { bits } => {
                let bytes = (*bits as usize).div_ceil(8);
                bytes.min(8)
            },
            Self::Float128 => 16,
            Self::Pointer(_) => 8,
            Self::Array(elem, _) => elem.align(),
            Self::Aggregate { .. } => 1,
            Self::Function { .. } => 8,
            Self::Vector(elem, _) => elem.align(),
            Self::Complex(inner) => inner.align(),
            Self::Imaginary(inner) => inner.align(),
        }
    }

    /// Check if this is a signed integer type.
    pub fn is_signed(&self) -> bool {
        matches!(self, Self::Integer { signed: true, .. })
    }

    /// Check if this is an unsigned integer type.
    pub fn is_unsigned(&self) -> bool {
        matches!(self, Self::Integer { signed: false, .. })
    }

    /// Check if this is a floating point type.
    pub fn is_floating_point(&self) -> bool {
        matches!(self, Self::Float { .. })
    }

    /// Check if this is an integer type.
    pub fn is_integer(&self) -> bool {
        matches!(self, Self::Integer { .. })
    }

    /// Check if this is a pointer type.
    pub fn is_pointer(&self) -> bool {
        matches!(self, Self::Pointer(..))
    }

    /// Check if this is an aggregate type.
    pub fn is_aggregate(&self) -> bool {
        matches!(self, Self::Aggregate { .. })
    }

    /// Check if this is a vector type.
    pub fn is_vector(&self) -> bool {
        matches!(self, Self::Vector(..))
    }

    /// Check if this is a complex number type.
    pub fn is_complex(&self) -> bool {
        matches!(self, Self::Complex(..))
    }

    /// Check if this is a void type.
    pub fn is_void(&self) -> bool {
        matches!(self, Self::Void)
    }

    /// Get the bit width for scalar types.
    pub fn bit_width(&self) -> Option<u16> {
        match self {
            Self::Integer { bits, .. } => Some(*bits),
            Self::Float { bits } => Some(*bits),
            Self::Vector(elem, _) => elem.bit_width(),
            _ => None,
        }
    }

    /// Get element type for compound types.
    pub fn element_type(&self) -> Option<&ValueType> {
        match self {
            Self::Pointer(inner) => Some(inner),
            Self::Array(inner, _) => Some(inner),
            Self::Vector(inner, _) => Some(inner),
            Self::Complex(inner) => Some(inner),
            Self::Imaginary(inner) => Some(inner),
            _ => None,
        }
    }

    /// Check if two types are compatible for comparison operations.
    pub fn compatible_with(&self, other: &ValueType) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}

// ============================================================================
// IMPLEMENTATION FOR TYPED EXPR
// ============================================================================

impl TypedExpr {
    /// Get byte representation size.
    pub fn bytes(&self) -> usize {
        self.ty.size()
    }

    /// Check if this expression is a constant.
    pub fn is_constant(&self) -> bool {
        matches!(self.kind, ExprKind::Constant)
    }

    /// Check if this is a literal value.
    pub fn is_literal(&self) -> bool {
        self.literal.is_some()
    }

    /// Get the literal value if this is a literal expression.
    pub fn literal_value(&self) -> Option<ExprLiteral> {
        self.literal
    }
}

// ============================================================================
// IMPLEMENTATION FOR EXPR LITERAL
// ============================================================================

impl ExprLiteral {
    /// Convert to i128 value.
    pub fn as_int(&self) -> i128 {
        match self {
            Self::Int(v) => *v,
            Self::Float(f) => f.as_f64() as i128,
        }
    }

    /// Convert to f64 value.
    pub fn as_float(&self) -> f64 {
        match self {
            Self::Int(v) => *v as f64,
            Self::Float(v) => v.as_f64(),
        }
    }

    /// Check if this is an integer literal.
    pub fn is_int(&self) -> bool {
        matches!(self, Self::Int(_))
    }

    /// Check if this is a float literal.
    pub fn is_float(&self) -> bool {
        matches!(self, Self::Float(_))
    }
}

impl Lowering {
    fn lower_type(&mut self, ty: &Type, span: &Span) -> ValueType {
        match ty {
            Type::Void => ValueType::Void,
            Type::Bool => ValueType::Integer {
                bits: 1,
                signed: false,
            },
            Type::Char | Type::SChar => ValueType::Integer {
                bits: 8,
                signed: true,
            },
            Type::UChar | Type::Char8 => ValueType::Integer {
                bits: 8,
                signed: false,
            },
            Type::Char16 => ValueType::Integer {
                bits: 16,
                signed: false,
            },
            Type::Char32 => ValueType::Integer {
                bits: 32,
                signed: false,
            },
            Type::Short => ValueType::Integer {
                bits: 16,
                signed: true,
            },
            Type::UShort => ValueType::Integer {
                bits: 16,
                signed: false,
            },
            Type::Int | Type::Enum(_, _, _) | Type::Fract | Type::Accum => ValueType::Integer {
                bits: 32,
                signed: true,
            },
            Type::UInt => ValueType::Integer {
                bits: 32,
                signed: false,
            },
            Type::Long | Type::LongLong => ValueType::Integer {
                bits: 64,
                signed: true,
            },
            Type::ULong | Type::ULongLong => ValueType::Integer {
                bits: 64,
                signed: false,
            },
            Type::Int128 => ValueType::Integer {
                bits: 128,
                signed: true,
            },
            Type::UInt128 => ValueType::Integer {
                bits: 128,
                signed: false,
            },
            Type::Float => ValueType::Float { bits: 32 },
            Type::Double => ValueType::Float { bits: 64 },
            Type::LongDouble => ValueType::Float { bits: 128 },
            Type::Float128 => ValueType::Float128,
            Type::Ptr(inner) => ValueType::Pointer(Box::new(self.lower_type(&inner.base, span))),
            Type::Array(inner, size) => ValueType::Array(
                Box::new(self.lower_type(&inner.base, span)),
                match size {
                    ast::ArraySize::Const(n) => Some(*n),
                    ast::ArraySize::Var(expr) => {
                        // VLA: try constant evaluation for compile-time
                        // determinable sizes (e.g. int a[2 * 3])
                        match const_eval::eval(expr) {
                            Ok(n) if n >= 0 => Some(n as usize),
                            _ => {
                                self.diagnostics.push(Diagnostic::warn(
                                    "W1012",
                                    "variable-length array size not determinable at compile time",
                                    span.clone(),
                                ));
                                None
                            },
                        }
                    },
                },
            ),
            Type::Struct(name, _, _, _) => ValueType::Aggregate {
                name: name.clone(),
                size: ty.size(),
                is_union: false,
            },
            Type::Union(name, _, _, _) => ValueType::Aggregate {
                name: name.clone(),
                size: ty.size(),
                is_union: true,
            },
            Type::Func(ret, params) => ValueType::Function {
                ret: Box::new(self.lower_type(&ret.base, span)),
                params: params
                    .iter()
                    .map(|p| self.lower_type(&p.base, span))
                    .collect(),
            },
            Type::Vector(inner, n) => {
                ValueType::Vector(Box::new(self.lower_type(&inner.base, span)), *n)
            },
            Type::Complex(inner) => {
                ValueType::Complex(Box::new(self.lower_type(&inner.base, span)))
            },
            Type::Imaginary(inner) => {
                ValueType::Imaginary(Box::new(self.lower_type(&inner.base, span)))
            },
            Type::Atomic(inner) => self.lower_type(&inner.base, span),
            Type::Named(name) => {
                // Try to resolve through pre-scanned type definitions
                if let Some(resolved) = self.type_defs.get(name).cloned() {
                    return self.lower_type(&resolved, span);
                }
                // Downgrade to warning: kernel code may have opaque types
                // that are never fully defined in the current TU.
                self.diagnostics.push(Diagnostic::warn(
                    "W1013",
                    format!("unresolved type '{}' in typed IR, assuming int", name),
                    span.clone(),
                ));
                ValueType::Integer {
                    bits: 32,
                    signed: true,
                }
            },
        }
    }

    fn lower_item(&mut self, item: &BlockItem) -> Option<TypedStmt> {
        match item {
            BlockItem::Stmt(stmt) => Some(self.lower_stmt(stmt)),
            BlockItem::Decl(Decl::Var(v)) => Some(TypedStmt {
                expressions: v
                    .init
                    .as_ref()
                    .map(|e| vec![self.lower_expr(e)])
                    .unwrap_or_default(),
                nested: Vec::new(),
                span: v.span.clone(),
            }),
            BlockItem::Decl(Decl::Func(f)) => Some(TypedStmt {
                expressions: Vec::new(),
                nested: f.body.as_ref().map_or_else(Vec::new, |b| {
                    b.items.iter().filter_map(|i| self.lower_item(i)).collect()
                }),
                span: f.span.clone(),
            }),
            BlockItem::Decl(_) => None,
        }
    }

    fn lower_stmt(&mut self, stmt: &Stmt) -> TypedStmt {
        let (expressions, nested, span) = match stmt {
            Stmt::Return { val, span } => (
                val.as_ref()
                    .map(|e| vec![self.lower_expr(e)])
                    .unwrap_or_default(),
                Vec::new(),
                span,
            ),
            Stmt::Expr { e, span } => (vec![self.lower_expr(e)], Vec::new(), span),
            Stmt::If {
                cond,
                then_,
                else_,
                span,
            } => {
                let mut nested = vec![self.lower_stmt(then_)];
                if let Some(other) = else_ {
                    nested.push(self.lower_stmt(other));
                }
                (vec![self.lower_expr(cond)], nested, span)
            },
            Stmt::While { cond, body, span } | Stmt::Do { cond, body, span } => {
                (vec![self.lower_expr(cond)], vec![self.lower_stmt(body)], span)
            },
            Stmt::For {
                init,
                cond,
                step,
                body,
                span,
            } => {
                let mut expressions = Vec::new();
                if let Some(e) = cond {
                    expressions.push(self.lower_expr(e));
                }
                if let Some(e) = step {
                    expressions.push(self.lower_expr(e));
                }
                let mut nested = Vec::new();
                if let Some(s) = init {
                    nested.push(self.lower_stmt(s));
                }
                nested.push(self.lower_stmt(body));
                (expressions, nested, span)
            },
            Stmt::Block { b, span } => {
                (Vec::new(), b.items.iter().filter_map(|i| self.lower_item(i)).collect(), span)
            },
            Stmt::Switch {
                expr, cases, span, ..
            } => (
                vec![self.lower_expr(expr)],
                cases
                    .iter()
                    .flat_map(|c| c.body.items.iter())
                    .filter_map(|i| self.lower_item(i))
                    .collect(),
                span,
            ),
            Stmt::GotoIndirect { expr, span } => (vec![self.lower_expr(expr)], Vec::new(), span),
            // C++ 零成本 EH: TypedStmt 是表达式导向的、无 kind 的中间层, 异常边
            // 不在这一层建模 (只有 x86_64 后端从 AST 生成代码时消费)。
            // 这里只保留语义载荷: 受保护调用降为其内部的调用表达式, 落地垫序言
            // 降为两个槽的写入表达式, 以便优化 pass 仍能看到副作用。
            Stmt::Invoke { call, span, .. } => {
                let inner = self.lower_stmt(call);
                (inner.expressions, inner.nested, span)
            },
            Stmt::EhLandingPad {
                object,
                selector,
                span,
                ..
            } => (vec![self.lower_expr(object), self.lower_expr(selector)], Vec::new(), span),
            Stmt::Label { stmt, span, .. } => (Vec::new(), vec![self.lower_stmt(stmt)], span),
            Stmt::AsmGoto {
                outputs,
                inputs,
                span,
                ..
            } => (
                outputs
                    .iter()
                    .chain(inputs)
                    .map(|e| self.lower_expr(e))
                    .collect(),
                Vec::new(),
                span,
            ),
            Stmt::Break { span } | Stmt::Continue { span } | Stmt::Goto { span, .. } => {
                (Vec::new(), Vec::new(), span)
            },
            Stmt::Fallthrough { span } => (Vec::new(), Vec::new(), span),
        };
        TypedStmt {
            expressions,
            nested,
            span: span.clone(),
        }
    }

    fn lower_expr(&mut self, expr: &Expr) -> TypedExpr {
        let span = expr_span(expr);
        let mut ty = self.lower_type(&expr.ty(), &span);
        let mut literal = None;
        let (kind, children) = match expr {
            Expr::Int { v, .. } => {
                literal = Some(ExprLiteral::Int(*v));
                (ExprKind::Constant, Vec::new())
            },
            Expr::Char { v, .. } => {
                literal = Some(ExprLiteral::Int(*v));
                (ExprKind::Constant, Vec::new())
            },
            Expr::Float { c, .. } => {
                literal = Some(ExprLiteral::Float(*c));
                (ExprKind::Constant, Vec::new())
            },
            Expr::ComplexLit { imag, .. } => {
                // 虚数字面量: 常量 (虚部值; 实部 0)
                literal = Some(ExprLiteral::Float(*imag));
                (ExprKind::Constant, Vec::new())
            },
            Expr::ComplexIntLit { imag, .. } => {
                // 整型虚数字面量: 常量 (虚部整数; 实部 0)
                literal = Some(ExprLiteral::Int(*imag));
                (ExprKind::Constant, Vec::new())
            },
            Expr::Real { e, .. } | Expr::Imag { e, .. } => {
                // 实/虚部提取: 按一元运算降级 (profile 用途, 值透传)
                (ExprKind::Unary(UnOp::Plus), vec![self.lower_expr(e)])
            },
            Expr::String { .. } => (ExprKind::Constant, Vec::new()),
            Expr::Ident { name, .. } => {
                // Static local 名字解析: 源名 -> mangled 名
                let resolved_name = if let Some(ref func) = self.current_func {
                    self.sl_map
                        .resolve(func, &name.name)
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| name.name.clone())
                } else {
                    name.name.clone()
                };
                (ExprKind::Local(resolved_name), Vec::new())
            },
            Expr::Binop { op, l, r, .. } => {
                (ExprKind::Binary(*op), vec![self.lower_expr(l), self.lower_expr(r)])
            },
            Expr::Unary { op, e, .. } => (ExprKind::Unary(*op), vec![self.lower_expr(e)]),
            Expr::Assign { l, r, .. } => {
                (ExprKind::Assign, vec![self.lower_expr(l), self.lower_expr(r)])
            },
            Expr::Cond {
                cond, then_, else_, ..
            } => (
                ExprKind::Conditional,
                vec![
                    self.lower_expr(cond),
                    self.lower_expr(then_),
                    self.lower_expr(else_),
                ],
            ),
            Expr::Cast { e, .. } => (ExprKind::Cast, vec![self.lower_expr(e)]),
            Expr::Call { callee, args, .. } => {
                match callee.ty() {
                    Type::Func(ret, _) => ty = self.lower_type(&ret.base, &span),
                    Type::Ptr(inner) => {
                        if let Type::Func(ret, _) = &inner.base {
                            ty = self.lower_type(&ret.base, &span);
                        }
                    },
                    _ => {},
                }
                let mut children = vec![self.lower_expr(callee)];
                children.extend(args.iter().map(|e| self.lower_expr(e)));
                (ExprKind::Call, children)
            },
            Expr::Index { arr, idx, .. } => {
                (ExprKind::Index, vec![self.lower_expr(arr), self.lower_expr(idx)])
            },
            Expr::Member {
                expr: base,
                field,
                is_arrow,
                ..
            } => {
                let aggregate = if *is_arrow {
                    match base.ty() {
                        Type::Ptr(inner) | Type::Array(inner, _) => inner.base.clone(),
                        other => other,
                    }
                } else {
                    base.ty()
                };
                if let Type::Struct(_, fields, _, _) | Type::Union(_, fields, _, _) = aggregate {
                    if let Some((found, _)) = ast::find_field(&fields, &field.name) {
                        ty = self.lower_type(&found.ty, &field.span);
                    } else if !fields.is_empty() {
                        // 降级为 warning: 内核复杂头文件中结构体可能不完整
                        self.diagnostics.push(Diagnostic::warn(
                            "W1009",
                            format!(
                                "unknown aggregate member '{}' (incomplete struct?)",
                                field.name
                            ),
                            field.span.clone(),
                        ));
                    }
                } else {
                    // 降级为 warning: 内核代码中隐式函数声明返回 int,
                    // 但后续可能对其结果做成员访问 (如 rcu_dereference(p)->x).
                    // 硬错误会阻断整个 TU 编译, 降级后 codegen 按 int 处理.
                    self.diagnostics.push(Diagnostic::warn(
                        "W1010",
                        "member access on non-struct/union type (implicit declaration?)",
                        span.clone(),
                    ));
                }
                (ExprKind::Member(field.name.clone()), vec![self.lower_expr(base)])
            },
            Expr::AddrOf { e, .. } => (ExprKind::Address, vec![self.lower_expr(e)]),
            Expr::Deref { e, .. } => {
                match e.ty() {
                    Type::Ptr(inner) | Type::Array(inner, _) => {
                        ty = self.lower_type(&inner.base, &span);
                    },
                    _ => {
                        // 降级为 warning: 隐式声明函数返回 int 但被解引用
                        // (如 NODE_DATA(nid)->x). 阻断编译不利于内核构建.
                        self.diagnostics.push(Diagnostic::warn(
                            "W1004",
                            "dereference of non-pointer type (implicit declaration?)",
                            span.clone(),
                        ));
                    },
                }
                (ExprKind::Dereference, vec![self.lower_expr(e)])
            },
            Expr::PostInc { e, .. }
            | Expr::PostDec { e, .. }
            | Expr::PreInc { e, .. }
            | Expr::PreDec { e, .. } => (ExprKind::Assign, vec![self.lower_expr(e)]),
            Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
                (ExprKind::Aggregate, lower_inits(self, inits))
            },
            Expr::StmtExpr { block, .. } => (
                ExprKind::ControlFlow,
                block
                    .items
                    .iter()
                    .filter_map(|item| match item {
                        BlockItem::Stmt(Stmt::Expr { e, .. }) => Some(self.lower_expr(e)),
                        BlockItem::Decl(Decl::Var(v)) => {
                            // Preserve local variable initializers
                            v.init.as_ref().map(|e| self.lower_expr(e))
                        },
                        BlockItem::Stmt(other) => {
                            // Lower other statements and collect their
                            // expressions (e.g. conditions, nested flows)
                            let typed = self.lower_stmt(other);
                            if typed.expressions.is_empty() {
                                None
                            } else {
                                // Wrap nested stmt as a control-flow child
                                Some(TypedExpr {
                                    ty: ValueType::Void,
                                    kind: ExprKind::ControlFlow,
                                    children: typed.expressions,
                                    span: typed.span,
                                    literal: None,
                                    quals: QualifierFlags::NONE,
                                })
                            }
                        },
                        _ => None,
                    })
                    .collect(),
            ),
            Expr::Comma { l, r, .. } => {
                (ExprKind::ControlFlow, vec![self.lower_expr(l), self.lower_expr(r)])
            },
            Expr::VaArg { ap, ref ty, .. } => {
                // 支持简单标识符和结构体成员访问 (如 state->ap)
                let ap_name = match ap.as_ref() {
                    Expr::Ident { name, .. } => name.name.clone(),
                    // 结构体成员访问: 使用成员名作为近似标识
                    Expr::Member { field, .. } => field.name.clone(),
                    _ => String::new(),
                };
                (
                    ExprKind::VaArg {
                        ap: ap_name,
                        arg_ty: self.lower_type(ty, &span),
                    },
                    vec![self.lower_expr(ap)],
                )
            },
            Expr::VaArgPack { .. } => (ExprKind::ControlFlow, Vec::new()),
            Expr::SizeofTy { ty: ref sty, .. } => {
                literal = Some(ExprLiteral::Int(sty.size() as i128));
                (ExprKind::SizeAlignOffset, Vec::new())
            },
            Expr::AlignofTy { ty: ref aty, .. } => {
                literal = Some(ExprLiteral::Int(aty.align() as i128));
                (ExprKind::SizeAlignOffset, Vec::new())
            },
            Expr::Offsetof {
                ty: ref oty,
                ref member,
                ..
            } => {
                let off = crate::const_eval::eval_offsetof(oty, member);
                literal = Some(ExprLiteral::Int(off as i128));
                (ExprKind::SizeAlignOffset, Vec::new())
            },
            Expr::SizeofExpr { e, .. } | Expr::AlignofExpr { e, .. } => {
                (ExprKind::SizeAlignOffset, vec![self.lower_expr(e)])
            },
            Expr::AsmExpr { .. } | Expr::LabelAddr { .. } => (ExprKind::Opaque, Vec::new()),
        };
        TypedExpr {
            ty,
            kind,
            children,
            span,
            literal,
            quals: QualifierFlags::NONE,
        }
    }
}

fn lower_inits(cx: &mut Lowering, inits: &[InitItem]) -> Vec<TypedExpr> {
    inits
        .iter()
        .flat_map(|item| match item {
            InitItem::Expr(e) => vec![cx.lower_expr(e)],
            InitItem::Designated { designator, value } => {
                let mut out = Vec::new();
                if let ast::Designator::Index(index) = designator {
                    out.push(cx.lower_expr(index));
                }
                out.push(cx.lower_expr(value));
                out
            },
        })
        .collect()
}

/// 检查表达式是否是合法的取址目标 (即 &expr 能产生静态初始化器).
/// 覆盖: ident, arr[const][const]..., compound_lit, member chains.
fn is_addr_target_ok(e: &Expr) -> bool {
    match e {
        Expr::Ident { .. } | Expr::String { .. } => true,
        Expr::Index { arr, idx, .. } => is_addr_target_ok(arr) && const_eval::eval(idx).is_ok(),
        Expr::Member { expr, .. } => is_addr_of_member_ok(expr),
        Expr::CompoundLit { inits, .. } => inits.iter().all(|item| match item {
            InitItem::Expr(e) => is_static_initializer(e),
            InitItem::Designated { value, .. } => is_static_initializer(value),
        }),
        _ => false,
    }
}

fn is_static_initializer(expr: &Expr) -> bool {
    if const_eval::eval(expr).is_ok() {
        return true;
    }
    // 浮点常量表达式 (如 __builtin_nan("") / __builtin_inf()) 折叠为
    // ConstValue::Float, 也是合法的静态初始化器。
    if matches!(
        const_eval::eval_full(expr, false),
        Ok(const_eval::ConstValue::Float(_))
    ) {
        return true;
    }
    // 地址常量 (ConstValue::Addr): 指针算术/成员偏移折叠成功即为
    // 链接期可重定位常量 (C11 6.6p9), 如 &((arr + 1)->field)
    if matches!(const_eval::eval_full(expr, false), Ok(const_eval::ConstValue::Addr { .. })) {
        return true;
    }
    match expr {
        Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::Char { .. }
        | Expr::String { .. }
        | Expr::LabelAddr { .. }
        // 复数常量字面量 (`1.0 + 2.0i` / `__builtin_complex` 的脱糖产物):
        // 两个分量都是编译期 FloatConst, `gen_global_init_list` 会按目标
        // 内型走 `complex_init_bytes` 逐分量编码 (gcc-dg torture/floatn-complex.h)。
        | Expr::ComplexLit { .. }
        | Expr::ComplexIntLit { .. } => true,
        Expr::Ident { .. } => true,
        Expr::SizeofTy { .. }
        | Expr::SizeofExpr { .. }
        | Expr::AlignofTy { .. }
        | Expr::AlignofExpr { .. }
        | Expr::Offsetof { .. } => true,
        Expr::Cast { e, .. }
        | Expr::Unary {
            op: UnOp::Neg | UnOp::Plus,
            e,
            ..
        } => is_static_initializer(e),
        Expr::Cond {
            cond, then_, else_, ..
        } => match const_eval::eval(cond) {
            Ok(v) if v != 0 => is_static_initializer(then_),
            Ok(_) => is_static_initializer(else_),
            Err(_) => false,
        },
        // &ident / &arr[const][const]... / &ident.field... / &(struct T){...}: 链接期可重定位常量
        Expr::AddrOf { e, .. } => is_addr_target_ok(e),
        // 数组成员隐式数组退化: init_uts_ns.name.machine (char[65]) 作为
        // 初始化器值等价 &init_uts_ns.name.machine[0], 是链接期可重定位
        // 地址常量 (与显式 & 形式一致)
        e @ Expr::Member { expr, .. } if matches!(e.ty(), Type::Array(..)) => {
            is_addr_of_member_ok(expr)
        },
        // 地址算术: 指针 + 整数 / 整数 + 指针 / 指针 - 整数
        // 也覆盖 (unsigned long)&var + sizeof(var) - const 等内核模式
        // 扩展: 也接受位运算/移位/比较等 (GCC 允许地址常量的这些运算)
        Expr::Binop { l, r, .. } => {
            // 优先尝试 const_eval (纯整数常量)
            if const_eval::eval(expr).is_ok() {
                return true;
            }
            // 地址算术 / 位运算 / 比较: 两边都是静态初始化器时允许
            is_static_initializer(l) && is_static_initializer(r)
        },
        // 逗号表达式: 右边必须是静态初始化器
        Expr::Comma { r, .. } => is_static_initializer(r),
        Expr::InitList { inits, .. } | Expr::CompoundLit { inits, .. } => {
            inits.iter().all(|item| {
                match item {
                    InitItem::Expr(e) => is_static_initializer(e),
                    InitItem::Designated { designator, value } => {
                        let index_ok = match designator {
                            ast::Designator::Field(_) => true,
                            ast::Designator::Index(e) => const_eval::eval(e).is_ok(),
                            ast::Designator::Range(f, l) => {
                                const_eval::eval(f).is_ok() && const_eval::eval(l).is_ok()
                            },
                            // 链式 designator: 全部 Index 都要能常量求值, Field 始终 OK
                            ast::Designator::Chain(items) => items.iter().all(|d| match d {
                                ast::Designator::Field(_) => true,
                                ast::Designator::Index(e) => const_eval::eval(e).is_ok(),
                                ast::Designator::Range(f, l) => {
                                    const_eval::eval(f).is_ok() && const_eval::eval(l).is_ok()
                                },
                                ast::Designator::Chain(_) => false,
                            }),
                        };
                        index_ok && is_static_initializer(value)
                    },
                }
            })
        },
        _ => false,
    }
}

/// 嵌套 `&ident.field...` 中基址是标识符的简单形式是链接期常量。
/// 例如 `&init_uts_ns.ns_unified_node` 或 `&a.b.c.d`。
/// 也接受 `(&x)->field` (等价 `x.field`) 与 `&(x.field)` 形式。
fn is_addr_of_member_ok(e: &Expr) -> bool {
    match e {
        Expr::Ident { .. } => true,
        Expr::Member { expr, .. } => is_addr_of_member_ok(expr),
        // cpumask_bits(&mask) 等宏展开为 (&mask)->bits: 先对基址取址再访问成员
        Expr::AddrOf { e, .. } => is_addr_of_member_ok(e),
        // arr[const].field...: 数组下标 + 成员访问链
        Expr::Index { arr, idx, .. } => {
            let base_ok = match arr.as_ref() {
                Expr::Ident { .. } => true,
                Expr::Member { expr, .. } => is_addr_of_member_ok(expr),
                // 嵌套索引
                Expr::Index { .. } => is_static_initializer(arr),
                _ => false,
            };
            base_ok && const_eval::eval(idx).is_ok()
        },
        // (type *)0x1000->field: 整数到指针的强制转换 (链接期地址常量)
        Expr::Cast { e, .. } => is_static_initializer(e),
        // (struct S){...}.field: 复合字面量成员
        Expr::CompoundLit { inits, .. } => inits.iter().all(|item| match item {
            InitItem::Expr(e) => is_static_initializer(e),
            InitItem::Designated { value, .. } => is_static_initializer(value),
        }),
        _ => false,
    }
}

fn expr_span(expr: &Expr) -> Span {
    match expr {
        Expr::Int { span, .. }
        | Expr::Float { span, .. }
        | Expr::ComplexLit { span, .. }
        | Expr::ComplexIntLit { span, .. }
        | Expr::Real { span, .. }
        | Expr::Imag { span, .. }
        | Expr::String { span, .. }
        | Expr::Char { span, .. }
        | Expr::Ident { span, .. }
        | Expr::Binop { span, .. }
        | Expr::Unary { span, .. }
        | Expr::Call { span, .. }
        | Expr::Assign { span, .. }
        | Expr::Cond { span, .. }
        | Expr::Cast { span, .. }
        | Expr::SizeofTy { span, .. }
        | Expr::SizeofExpr { span, .. }
        | Expr::Index { span, .. }
        | Expr::Member { span, .. }
        | Expr::AddrOf { span, .. }
        | Expr::Deref { span, .. }
        | Expr::PostInc { span, .. }
        | Expr::PostDec { span, .. }
        | Expr::PreInc { span, .. }
        | Expr::PreDec { span, .. }
        | Expr::CompoundLit { span, .. }
        | Expr::InitList { span, .. }
        | Expr::AlignofTy { span, .. }
        | Expr::AlignofExpr { span, .. }
        | Expr::StmtExpr { span, .. }
        | Expr::Comma { span, .. }
        | Expr::AsmExpr { span, .. }
        | Expr::LabelAddr { span, .. }
        | Expr::VaArg { span, .. }
        | Expr::VaArgPack { span, .. }
        | Expr::Offsetof { span, .. } => span.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{BlockItem, Decl, Stmt};

    #[test]
    fn lowers_functions_and_typed_expressions() {
        let (program, diagnostics) =
            crate::parser::parse(b"int add(int a, int b) { return a + b; }", "ir-test.c");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let (module, _) = lower(program).expect("lowering should succeed");
        validate(&module).expect("validation should succeed");
        assert_eq!(module.functions.len(), 1);
        assert!(matches!(
            module.functions[0].body[0].expressions[0].kind,
            ExprKind::Binary(BinOp::Add)
        ));
        assert_eq!(
            module.functions[0].body[0].expressions[0].ty,
            ValueType::Integer {
                bits: 32,
                signed: true
            }
        );
    }

    #[test]
    fn static_key_residual_decl_becomes_extern_global() {
        let src = b"_DECLARE_STATIC_KEY_0(init_on_alloc);\nint main(void) { return 0; }\n";
        let (program, diagnostics) = crate::parser::parse(src, "static-key.c");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(program.decls.len(), 2);
        if let Decl::Var(v) = &program.decls[0] {
            assert!(v.is_extern);
            assert_eq!(v.name.name, "init_on_alloc");
            assert!(matches!(v.ty, Type::Struct(ref n, _, _, _) if n == "static_key_false"));
        } else {
            panic!("expected VarDecl, got {:?}", program.decls[0]);
        }
        let (module, _) = lower(program).expect("static key should lower");
        validate(&module).expect("static key global should validate");
        assert_eq!(module.globals.len(), 1);
        assert_eq!(module.globals[0].name, "init_on_alloc");
    }

    #[test]
    fn static_key_is_enabled_form_becomes_extern_global() {
        let src =
            b"_DECLARE_STATIC_KEY_IS_ENABLED(CFG)(init_on_alloc);\nint main(void) { return 0; }\n";
        let (program, diagnostics) = crate::parser::parse(src, "static-key-is-enabled.c");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        if let Decl::Var(v) = &program.decls[0] {
            assert!(v.is_extern);
            assert_eq!(v.name.name, "init_on_alloc");
        } else {
            panic!("expected VarDecl");
        }
        let _ = lower(program).expect("IS_ENABLED static key form should lower");
    }

    #[test]
    fn vector_global_initializer_is_static() {
        let src = b"typedef int v4si __attribute__((vector_size(16)));\n\
                     v4si g = {1, 2, 3, 4};\n\
                     int main(void) { return 0; }\n";
        let (program, _) = crate::parser::parse(src, "vector-global.c");
        let _ = lower(program).expect("vector global init should be constant");
    }

    #[test]
    fn non_constant_global_init_reports_e1006() {
        let src = b"int g = main();\nint main(void) { return 0; }\n";
        let (program, _) = crate::parser::parse(src, "e1006.c");
        let err = lower(program).expect_err("runtime call init must fail");
        assert!(err.iter().any(|d| d.code == "E1006"), "{err:?}");
    }

    #[test]
    fn float_mod_lowers_and_validates() {
        let src = b"int main(void) { float a = 1, b = 2; return (int)(a % b); }\n";
        let (program, _) = crate::parser::parse(src, "float_mod.c");
        let (module, _) = lower(program).expect("lower float mod");
        validate(&module).expect("float mod should pass validation");
    }

    #[test]
    fn vector_bitwise_or_validates() {
        let src = b"typedef int v4si __attribute__((vector_size(16)));\n\
                     int main(void) { v4si a, b; a | b; return 0; }\n";
        let (program, _) = crate::parser::parse(src, "vector-or.c");
        let (module, _) = lower(program).expect("lower vector or");
        validate(&module).expect("vector or should validate");
    }

    #[test]
    fn vector_cast_to_int_reports_e1007() {
        let src = b"typedef int v4si __attribute__((vector_size(16)));\n\
                     int main(void) { v4si v = {1,2,3,4}; return (int)v; }\n";
        let (program, _) = crate::parser::parse(src, "vector-cast.c");
        let (module, _) = lower(program).expect("lower despite vector cast");
        let err = validate(&module).expect_err("vector cast must fail validation");
        assert!(err.iter().any(|d| d.code == "E1007"), "{err:?}");
    }

    #[test]
    fn vector_assign_binop_validates() {
        let src = b"typedef int v4si __attribute__((vector_size(16)));\n\
                     int main(void) { v4si a={1,2,3,4}, b={5,6,7,8}; a = a + b; return 0; }\n";
        let (program, _) = crate::parser::parse(src, "vector-assign.c");
        let (module, _) = lower(program).expect("vector assign binop should lower");
        validate(&module).expect("vector assign from binop should validate");
    }

    #[test]
    fn vector_cast_from_initializer_validates() {
        let src = b"typedef int v4si __attribute__((vector_size(16)));\n\
                     int main(void) { v4si v = (v4si){1,2,3,4}; return v[0]; }\n";
        let (program, _) = crate::parser::parse(src, "vector-cast-init.c");
        let (module, _) = lower(program).expect("vector initializer cast should lower");
        validate(&module).expect("vector initializer cast should validate");
    }

    #[test]
    fn same_size_vector_and_complex_casts_validate() {
        let src = b"typedef int v2si __attribute__((vector_size(8)));\n\
                     int main(void) {\n\
                         v2si v = {0x3f800000, 0x40000000};\n\
                         float _Complex z = (float _Complex)v;\n\
                         v2si w = (v2si)z;\n\
                         unsigned long long bits = (unsigned long long)w;\n\
                         return (int)bits;\n\
                     }\n";
        let (program, _) = crate::parser::parse(src, "complex-vector-cast.c");
        let (module, _) = lower(program).expect("same-size vector/complex casts should lower");
        validate(&module).expect("same-size vector/complex casts should validate");
    }

    #[test]
    fn same_size_vector_scalar_casts_validate() {
        let src = b"typedef unsigned char v8qi __attribute__((vector_size(8)));\n\
                     int main(void) {\n\
                         v8qi v = {1,2,3,4,5,6,7,8};\n\
                         unsigned long long bits = (unsigned long long)v;\n\
                         v8qi w = (v8qi)bits;\n\
                         return w[0];\n\
                     }\n";
        let (program, _) = crate::parser::parse(src, "vector-scalar-cast.c");
        let (module, _) = lower(program).expect("same-size vector/scalar casts should lower");
        validate(&module).expect("same-size vector/scalar casts should validate");
    }

    #[test]
    fn different_size_vector_scalar_cast_reports_e1007() {
        let src = b"typedef int v4si __attribute__((vector_size(16)));\n\
                     int main(void) { v4si v = {1,2,3,4}; return (long long)v; }\n";
        let (program, _) = crate::parser::parse(src, "vector-scalar-size-mismatch.c");
        let (module, _) = lower(program).expect("lower despite vector/scalar mismatch");
        let err = validate(&module).expect_err("different-size vector/scalar cast must fail");
        assert!(err.iter().any(|d| d.code == "E1007"), "{err:?}");
    }

    #[test]
    fn unsupported_same_size_vector_pointer_cast_reports_e1007() {
        let src = b"typedef long long v1di __attribute__((vector_size(8)));\n\
                     int main(void) { v1di v = {1}; return (long)(void *)v; }\n";
        let (program, _) = crate::parser::parse(src, "vector-pointer-cast.c");
        let (module, _) = lower(program).expect("lower despite vector/pointer cast");
        let err = validate(&module).expect_err("vector/pointer cast must fail validation");
        assert!(err.iter().any(|d| d.code == "E1007"), "{err:?}");
    }

    #[test]
    fn different_size_complex_vector_cast_reports_e1007_and_e1008() {
        let src = b"typedef int v4si __attribute__((vector_size(16)));\n\
                     int main(void) { float _Complex z = 1; v4si v = (v4si)z; return v[0]; }\n";
        let (program, _) = crate::parser::parse(src, "complex-vector-size-mismatch.c");
        let (module, _) = lower(program).expect("lower despite complex/vector mismatch");
        let err = validate(&module).expect_err("different-size complex/vector cast must fail");
        assert!(err.iter().any(|d| d.code == "E1007"), "{err:?}");
        assert!(err.iter().any(|d| d.code == "E1008"), "{err:?}");
    }

    #[test]
    fn unsupported_complex_int128_cast_reports_e1008() {
        let src = b"int main(void) { _Complex double z = 1; return (int)((__int128)z); }\n";
        let (program, _) = crate::parser::parse(src, "complex-int128-cast.c");
        let (module, _) = lower(program).expect("lower despite complex/int128 cast");
        let err = validate(&module).expect_err("complex to int128 cast must fail validation");
        assert!(err.iter().any(|d| d.code == "E1008"), "{err:?}");
    }

    #[test]
    fn complex_to_signed_integer_casts_validate() {
        let src = b"int main(void) {\n\
                         _Complex double z = 1;\n\
                         return (signed char)z + (short)z + (int)z + (long)z;\n\
                     }\n";
        let (program, _) = crate::parser::parse(src, "complex-signed-int-cast.c");
        let (module, _) = lower(program).expect("complex to signed integer casts should lower");
        validate(&module).expect("complex to signed integer casts should validate");
    }

    #[test]
    fn float_to_pointer_cast_reports_e1003() {
        let src = b"int main(void) { float f = 1; return (int)(long)(void *)f; }\n";
        let (program, _) = crate::parser::parse(src, "float-cast.c");
        let (module, _) = lower(program).expect("lower despite float pointer cast");
        let err = validate(&module).expect_err("float pointer cast must fail validation");
        assert!(err.iter().any(|d| d.code == "E1003"), "{err:?}");
    }

    #[test]
    fn vector_unary_plus_validates() {
        let src = b"typedef int v4si __attribute__((vector_size(16)));\n\
                     int main(void) { v4si v = {1,2,3,4}; +v; return 0; }\n";
        let (program, _) = crate::parser::parse(src, "vector-unary.c");
        let (module, _) = lower(program).expect("lower vector unary plus");
        validate(&module).expect("vector unary plus should validate");
    }

    #[test]
    fn complex_to_unsigned_cast_reports_e1008() {
        let src = b"int main(void) { _Complex float z = 1; return (unsigned)z; }\n";
        let (program, _) = crate::parser::parse(src, "complex-cast.c");
        let (module, _) = lower(program).expect("lower despite complex unsigned cast");
        let err = validate(&module).expect_err("complex unsigned cast must fail validation");
        assert!(err.iter().any(|d| d.code == "E1008"), "{err:?}");
    }

    #[test]
    fn float_bitwise_or_reports_e1003() {
        let src = b"int main(void) { float a = 1, b = 2; return (int)(a | b); }\n";
        let (program, _) = crate::parser::parse(src, "e1003.c");
        let (module, _) = lower(program).expect("lower despite float or");
        let err = validate(&module).expect_err("float or must fail validation");
        assert!(err.iter().any(|d| d.code == "E1003"), "{err:?}");
    }

    #[test]
    fn complex_bitwise_or_reports_e1008() {
        let src = b"int main(void) { _Complex float a = 1, b = 2; return (int)(a | b); }\n";
        let (program, _) = crate::parser::parse(src, "e1008.c");
        let (module, _) = lower(program).expect("lower despite complex or");
        let err = validate(&module).expect_err("complex or must fail validation");
        assert!(err.iter().any(|d| d.code == "E1008"), "{err:?}");
    }

    #[test]
    fn resync_rebuilds_ir_after_ast_change() {
        let src = b"int main(void) { return 2 + 3; }\n";
        let (program, _) = crate::parser::parse(src, "resync.c");
        let (mut module, _) = lower(program).expect("lower");
        validate(&module).expect("initial validate");
        assert!(matches!(
            module.functions[0].body[0].expressions[0].kind,
            ExprKind::Binary(BinOp::Add)
        ));
        if let Some(body) = module.source_mut().decls.iter_mut().find_map(|d| {
            if let Decl::Func(f) = d {
                f.body.as_mut()
            } else {
                None
            }
        }) {
            if let BlockItem::Stmt(Stmt::Return {
                val: Some(e), span, ..
            }) = &mut body.items[0]
            {
                *e = Expr::Int {
                    v: 5,
                    span: span.clone(),
                    ty: Type::Int,
                };
            }
        }
        resync(&mut module).expect("resync");
        validate(&module).expect("validate after resync");
        assert_eq!(module.functions[0].body[0].expressions[0].literal, Some(ExprLiteral::Int(5)));
    }

    // 5.1: type_info() exposes structured type definitions to IR consumers.
    #[test]
    fn type_info_exposes_struct_layout() {
        let src = b"struct point { int x; int y; };\n\
                     int main(void) { return 0; }\n";
        let (program, _) = crate::parser::parse(src, "type-info-struct.c");
        let (mut module, _) = lower(program).expect("lower");
        let info = module.type_info();
        assert!(!info.is_empty(), "type_info should not be empty");
        let found = info
            .iter()
            .any(|t| matches!(t, TypeDefInfo::Struct { name, .. } if name == "struct point"));
        assert!(found, "struct point should be in type_info");
    }

    #[test]
    fn type_info_exposes_union_and_enum() {
        let src = b"union data { int i; float f; };\n\
                     enum color { RED = 0, GREEN = 1, BLUE = 2 };\n\
                     int main(void) { return 0; }\n";
        let (program, _) = crate::parser::parse(src, "type-info-union-enum.c");
        let (mut module, _) = lower(program).expect("lower");
        let info = module.type_info();
        let has_union = info
            .iter()
            .any(|t| matches!(t, TypeDefInfo::Union { name, .. } if name == "union data"));
        let has_enum = info
            .iter()
            .any(|t| matches!(t, TypeDefInfo::Enum { name, .. } if name == "enum color"));
        assert!(has_union, "union data should be in type_info");
        assert!(has_enum, "enum color should be in type_info");
    }

    #[test]
    fn type_info_exposes_typedef() {
        let src = b"typedef unsigned long size_t;\n\
                     int main(void) { return 0; }\n";
        let (program, _) = crate::parser::parse(src, "type-info-typedef.c");
        let (mut module, _) = lower(program).expect("lower");
        let info = module.type_info();
        let has_td = info
            .iter()
            .any(|t| matches!(t, TypeDefInfo::Typedef { name, .. } if name == "size_t"));
        assert!(has_td, "typedef size_t should be in type_info");
    }

    #[test]
    fn find_aggregate_returns_struct_fields() {
        let src = b"struct pair { int a; long b; };\n\
                     int main(void) { return 0; }\n";
        let (program, _) = crate::parser::parse(src, "find-agg.c");
        let (mut module, _) = lower(program).expect("lower");
        let agg = module.find_aggregate("struct pair");
        assert!(agg.is_some(), "find_aggregate should find struct pair");
        if let Some(TypeDefInfo::Struct {
            fields,
            size,
            align,
            ..
        }) = agg
        {
            assert_eq!(fields.len(), 2);
            assert_eq!(fields[0].name, "a");
            assert_eq!(fields[1].name, "b");
            assert!(*size > 0);
            assert!(*align > 0);
        } else {
            panic!("expected Struct variant");
        }
    }

    #[test]
    fn resync_invalidates_type_info_cache() {
        let src = b"struct foo { int x; };\nint main(void) { return 0; }\n";
        let (program, _) = crate::parser::parse(src, "resync-cache.c");
        let (mut module, _) = lower(program).expect("lower");
        // Populate cache
        let info1 = module.type_info().len();
        assert!(info1 > 0);
        // resync should invalidate and rebuild
        resync(&mut module).expect("resync");
        let info2 = module.type_info().len();
        assert_eq!(info1, info2, "type_info count should match after resync");
    }

    #[test]
    fn static_locals_appear_in_ir_globals_with_correct_fields() {
        let src = b"\
int fa(void) { static int x; return ++x; }
int fb(void) { static int x = 42; return x; }
int main(void) { return fa() + fb(); }
";
        let (program, diag) = crate::parser::parse(src, "static-local-ir.c");
        assert!(diag.is_empty(), "{diag:?}");
        let (module, _) = lower(program).expect("lowering should succeed");
        validate(&module).expect("validation should succeed");
        // File-level globals: none in this test
        // Static locals: 2 (fa::x and fb::x)
        let static_globals: Vec<&Global> = module
            .globals
            .iter()
            .filter(|g| g.is_static_local)
            .collect();
        assert_eq!(static_globals.len(), 2, "expected 2 static locals in IR globals");
        // Verify first static local (fa::x)
        let fa_x = static_globals
            .iter()
            .find(|g| g.owner_function.as_deref() == Some("fa"))
            .expect("fa should own a static local");
        assert!(fa_x.name.starts_with(".Lsl"), "mangled name should have .Lsl prefix");
        assert!(fa_x.name.contains("fa"), "mangled name should contain func name");
        assert!(!fa_x.is_tls);
        // Verify second static local (fb::x)
        let fb_x = static_globals
            .iter()
            .find(|g| g.owner_function.as_deref() == Some("fb"))
            .expect("fb should own a static local");
        assert!(fb_x.name.starts_with(".Lsl"));
        assert!(fb_x.name.contains("fb"));
        // fb::x has initializer (42), so it should have an IR initializer
        assert!(
            fb_x.initializer.is_some(),
            "initialized static local should have IR initializer"
        );
        // fa::x is uninitialized, so no IR initializer
        assert!(
            fa_x.initializer.is_none(),
            "uninitialized static local should have no IR initializer"
        );
    }

    #[test]
    fn static_locals_validation_rejects_missing_owner() {
        // Construct a Global with is_static_local but no owner_function
        let bad_global = Global {
            name: ".Lsl0.bad.x".to_string(),
            ty: ValueType::Integer {
                bits: 32,
                signed: true,
            },
            initializer: None,
            span: crate::diagnostics::Span::point("", 0, 0),
            is_tls: false,
            is_static_local: true,
            owner_function: None, // should be Some for static local
            alignment: 4,
            section: None,
        };
        let module = Module {
            functions: vec![],
            globals: vec![bad_global],
            type_defs: vec![],
            type_info_cache: Vec::new(),
            source: crate::ast::Program {
                decls: vec![],
                fenv_access: false,
                fp_contract: false,
                cx_limited_range: false,
            },
        };
        let err = validate(&module).expect_err("should reject static local without owner");
        assert!(err.iter().any(|d| d.code == "E1090"), "expected E1090, got: {err:?}");
    }

    #[test]
    fn static_locals_validation_rejects_non_mangled_name() {
        let bad_global = Global {
            name: "plain_name".to_string(), // should start with .Lsl
            ty: ValueType::Integer {
                bits: 32,
                signed: true,
            },
            initializer: None,
            span: crate::diagnostics::Span::point("", 0, 0),
            is_tls: false,
            is_static_local: true,
            owner_function: Some("func".to_string()),
            alignment: 4,
            section: None,
        };
        let module = Module {
            functions: vec![],
            globals: vec![bad_global],
            type_defs: vec![],
            type_info_cache: Vec::new(),
            source: crate::ast::Program {
                decls: vec![],
                fenv_access: false,
                fp_contract: false,
                cx_limited_range: false,
            },
        };
        let err = validate(&module).expect_err("should reject non-mangled static local");
        assert!(err.iter().any(|d| d.code == "E1091"), "expected E1091, got: {err:?}");
    }
}
