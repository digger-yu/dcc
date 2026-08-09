//! Parser AST 与机器码后端之间的 typed IR。
//!
//! 该层保留 AST 作为后端迁移期间的语义载荷，同时建立独立、可验证的
//! 类型化函数/表达式图。后端入口只接受通过验证的 `Module`。

use crate::ast::{self, BinOp, BlockItem, Decl, Expr, InitItem, Stmt, Type, UnOp};
use crate::const_eval;
use crate::diagnostics::{Diagnostic, Severity, Span};

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
}

/// 编译期可确定的标量字面量（供多架构 IR lowering 使用）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExprLiteral {
    Int(i128),
    Float(f64),
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
    VaArg {
        ap: String,
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
}

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

#[derive(Debug, Clone)]
pub struct Global {
    pub name: String,
    pub ty: ValueType,
    pub initializer: Option<TypedExpr>,
    pub span: Span,
    pub is_tls: bool,
}

#[derive(Debug, Clone)]
pub struct Module {
    pub functions: Vec<Function>,
    pub globals: Vec<Global>,
    source: ast::Program,
}

impl Module {
    pub(crate) fn source(&self) -> &ast::Program {
        &self.source
    }
    pub(crate) fn source_mut(&mut self) -> &mut ast::Program {
        &mut self.source
    }
}

/// Lower AST to IR.
///
/// Returns `Ok((module, warnings))` on success; warnings may be non-empty
/// (e.g. implicit-function-declaration fallback).
/// Returns `Err(errors)` only when there are actual `Severity::Error`
/// diagnostics that prevent further compilation.
pub fn lower(source: ast::Program) -> Result<(Module, Vec<Diagnostic>), Vec<Diagnostic>> {
    let mut cx = Lowering {
        diagnostics: Vec::new(),
    };
    let mut functions = Vec::new();
    let mut globals = Vec::new();
    for decl in &source.decls {
        match decl {
            Decl::Func(f) => {
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
            }
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
                });
            }
            _ => {}
        }
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

/// 与 codegen SIMD 向量路径一致的二元运算符集合。
const VECTOR_BINOPS: &[BinOp] = &[BinOp::Add, BinOp::Sub, BinOp::Mul];

/// 与 codegen 复数路径一致的二元运算符集合。
const COMPLEX_BINOPS: &[BinOp] = &[BinOp::Add, BinOp::Sub, BinOp::Mul, BinOp::Div];

fn is_float_ty(ty: &ValueType) -> bool {
    matches!(ty, ValueType::Float { .. })
}

fn is_vector_ty(ty: &ValueType) -> bool {
    matches!(ty, ValueType::Vector(_, _))
}

fn is_complex_ty(ty: &ValueType) -> bool {
    matches!(ty, ValueType::Complex(_))
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
            if vector && !VECTOR_BINOPS.contains(&op) {
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
        }
        ExprKind::Unary(op) => {
            if let Some(operand) = expr.children.first() {
                if is_float_ty(&operand.ty) && !matches!(op, UnOp::Neg | UnOp::Plus) {
                    diagnostics.push(Diagnostic::err(
                        "E1003",
                        format!("unsupported floating-point unary operation {:?}", op),
                        expr.span.clone(),
                    ));
                }
                if is_vector_ty(&operand.ty) {
                    diagnostics.push(Diagnostic::err(
                        "E1007",
                        format!("unsupported vector unary operation {:?}", op),
                        expr.span.clone(),
                    ));
                }
                if is_complex_ty(&operand.ty) && !matches!(op, UnOp::Neg | UnOp::Plus) {
                    diagnostics.push(Diagnostic::err(
                        "E1008",
                        format!("unsupported complex unary operation {:?}", op),
                        expr.span.clone(),
                    ));
                }
            }
        }
        ExprKind::Assign if expr.children.len() >= 2 => {
            let lhs = &expr.children[0];
            let rhs = &expr.children[1];
            if is_vector_ty(&lhs.ty)
                && !is_vector_ty(&rhs.ty)
                && !matches!(
                    rhs.kind,
                    ExprKind::Aggregate | ExprKind::Cast | ExprKind::Binary(_)
                )
            {
                diagnostics.push(Diagnostic::err(
                    "E1007",
                    "vector assignment requires a vector initializer or compatible cast",
                    expr.span.clone(),
                ));
            }
            if is_complex_ty(&lhs.ty)
                && !is_complex_ty(&rhs.ty)
                && !matches!(rhs.kind, ExprKind::Cast | ExprKind::Binary(_))
            {
                diagnostics.push(Diagnostic::err(
                    "E1008",
                    "complex assignment requires a complex expression or cast",
                    expr.span.clone(),
                ));
            }
        }
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
        }
        ExprKind::Cast if let Some(src) = expr.children.first() => {
            if is_vector_ty(&src.ty) && !is_vector_ty(&expr.ty) {
                diagnostics.push(Diagnostic::err(
                    "E1007",
                    "unsupported cast from vector type",
                    expr.span.clone(),
                ));
            }
            if is_complex_ty(&src.ty) && is_vector_ty(&expr.ty) {
                diagnostics.push(Diagnostic::err(
                    "E1008",
                    "unsupported cast from complex to vector",
                    expr.span.clone(),
                ));
            }
        }
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
        }
        _ => {}
    }
    for child in &expr.children {
        validate_expr(child, diagnostics);
    }
}

struct Lowering {
    diagnostics: Vec<Diagnostic>,
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
            Type::UChar => ValueType::Integer {
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
            Type::Ptr(inner) => ValueType::Pointer(Box::new(self.lower_type(inner, span))),
            Type::Array(inner, size) => ValueType::Array(
                Box::new(self.lower_type(inner, span)),
                match size {
                    ast::ArraySize::Const(n) => Some(*n),
                    _ => None,
                },
            ),
            Type::Struct(name, _) => ValueType::Aggregate {
                name: name.clone(),
                size: ty.size(),
                is_union: false,
            },
            Type::Union(name, _) => ValueType::Aggregate {
                name: name.clone(),
                size: ty.size(),
                is_union: true,
            },
            Type::Func(ret, params) => ValueType::Function {
                ret: Box::new(self.lower_type(ret, span)),
                params: params.iter().map(|p| self.lower_type(p, span)).collect(),
            },
            Type::Vector(inner, n) => ValueType::Vector(Box::new(self.lower_type(inner, span)), *n),
            Type::Complex(inner) => ValueType::Complex(Box::new(self.lower_type(inner, span))),
            Type::Atomic(inner) => self.lower_type(inner, span),
            Type::Named(name) => {
                self.diagnostics.push(Diagnostic::err(
                    "E1001",
                    format!("unresolved type '{}' reached typed IR", name),
                    span.clone(),
                ));
                ValueType::Void
            }
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
            }
            Stmt::While { cond, body, span } | Stmt::Do { cond, body, span } => (
                vec![self.lower_expr(cond)],
                vec![self.lower_stmt(body)],
                span,
            ),
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
            }
            Stmt::Block { b, span } => (
                Vec::new(),
                b.items.iter().filter_map(|i| self.lower_item(i)).collect(),
                span,
            ),
            Stmt::Switch { expr, cases, span } => (
                vec![self.lower_expr(expr)],
                cases
                    .iter()
                    .flat_map(|c| c.body.items.iter())
                    .filter_map(|i| self.lower_item(i))
                    .collect(),
                span,
            ),
            Stmt::GotoIndirect { expr, span } => (vec![self.lower_expr(expr)], Vec::new(), span),
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
            }
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
            }
            Expr::Char { v, .. } => {
                literal = Some(ExprLiteral::Int(*v as i128));
                (ExprKind::Constant, Vec::new())
            }
            Expr::Float { v, .. } => {
                literal = Some(ExprLiteral::Float(*v));
                (ExprKind::Constant, Vec::new())
            }
            Expr::ComplexLit { imag, .. } => {
                // 虚数字面量: 常量 (虚部值; 实部 0)
                literal = Some(ExprLiteral::Float(*imag));
                (ExprKind::Constant, Vec::new())
            }
            Expr::Real { e, .. } | Expr::Imag { e, .. } => {
                // 实/虚部提取: 按一元运算降级 (profile 用途, 值透传)
                (ExprKind::Unary(UnOp::Plus), vec![self.lower_expr(e)])
            }
            Expr::String { .. } => (ExprKind::Constant, Vec::new()),
            Expr::Ident { name, .. } => (ExprKind::Local(name.name.clone()), Vec::new()),
            Expr::Binop { op, l, r, .. } => (
                ExprKind::Binary(*op),
                vec![self.lower_expr(l), self.lower_expr(r)],
            ),
            Expr::Unary { op, e, .. } => (ExprKind::Unary(*op), vec![self.lower_expr(e)]),
            Expr::Assign { l, r, .. } => (
                ExprKind::Assign,
                vec![self.lower_expr(l), self.lower_expr(r)],
            ),
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
                    Type::Func(ret, _) => ty = self.lower_type(&ret, &span),
                    Type::Ptr(inner) => {
                        if let Type::Func(ret, _) = inner.as_ref() {
                            ty = self.lower_type(ret, &span);
                        }
                    }
                    _ => {}
                }
                let mut children = vec![self.lower_expr(callee)];
                children.extend(args.iter().map(|e| self.lower_expr(e)));
                (ExprKind::Call, children)
            }
            Expr::Index { arr, idx, .. } => (
                ExprKind::Index,
                vec![self.lower_expr(arr), self.lower_expr(idx)],
            ),
            Expr::Member {
                expr: base,
                field,
                is_arrow,
                ..
            } => {
                let aggregate = if *is_arrow {
                    match base.ty() {
                        Type::Ptr(inner) => *inner,
                        other => other,
                    }
                } else {
                    base.ty()
                };
                if let Type::Struct(_, fields) | Type::Union(_, fields) = aggregate {
                    if let Some((found, _)) = ast::find_field(&fields, &field.name) {
                        ty = self.lower_type(&found.ty, &field.span);
                    } else if !fields.is_empty() {
                        // 降级为 warning: 内核复杂头文件中结构体可能不完整
                        self.diagnostics.push(Diagnostic::warn(
                            "W1009",
                            format!("unknown aggregate member '{}' (incomplete struct?)", field.name),
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
                (
                    ExprKind::Member(field.name.clone()),
                    vec![self.lower_expr(base)],
                )
            }
            Expr::AddrOf { e, .. } => (ExprKind::Address, vec![self.lower_expr(e)]),
            Expr::Deref { e, .. } => {
                match e.ty() {
                    Type::Ptr(inner) | Type::Array(inner, _) => {
                        ty = self.lower_type(&inner, &span);
                    }
                    _ => {
                        // 降级为 warning: 隐式声明函数返回 int 但被解引用
                        // (如 NODE_DATA(nid)->x). 阻断编译不利于内核构建.
                        self.diagnostics.push(Diagnostic::warn(
                            "W1004",
                            "dereference of non-pointer type (implicit declaration?)",
                            span.clone(),
                        ));
                    }
                }
                (ExprKind::Dereference, vec![self.lower_expr(e)])
            }
            Expr::PostInc { e, .. }
            | Expr::PostDec { e, .. }
            | Expr::PreInc { e, .. }
            | Expr::PreDec { e, .. } => (ExprKind::Assign, vec![self.lower_expr(e)]),
            Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
                (ExprKind::Aggregate, lower_inits(self, inits))
            }
            Expr::StmtExpr { block, .. } => (
                ExprKind::ControlFlow,
                block
                    .items
                    .iter()
                    .filter_map(|item| match item {
                        BlockItem::Stmt(Stmt::Expr { e, .. }) => Some(self.lower_expr(e)),
                        _ => None,
                    })
                    .collect(),
            ),
            Expr::Comma { l, r, .. } => (
                ExprKind::ControlFlow,
                vec![self.lower_expr(l), self.lower_expr(r)],
            ),
            Expr::VaArg { ap, .. } => {
                if !matches!(ap.as_ref(), Expr::Ident { .. }) {
                    self.diagnostics.push(Diagnostic::err(
                        "E1005",
                        "va_arg currently requires a va_list object",
                        span.clone(),
                    ));
                }
                // M25: 用独立 ExprKind 保留 ap 名, 避免跟 f() 误判
                let ap_name = if let Expr::Ident { name, .. } = ap.as_ref() {
                    name.name.clone()
                } else {
                    String::new()
                };
                (ExprKind::VaArg { ap: ap_name }, Vec::new())
            }
            Expr::SizeofTy { .. } | Expr::AlignofTy { .. } | Expr::Offsetof { .. } => {
                (ExprKind::SizeAlignOffset, Vec::new())
            }
            Expr::SizeofExpr { e, .. } | Expr::AlignofExpr { e, .. } => {
                (ExprKind::SizeAlignOffset, vec![self.lower_expr(e)])
            }
            Expr::AsmExpr { .. } | Expr::LabelAddr { .. } => (ExprKind::Opaque, Vec::new()),
        };
        TypedExpr {
            ty,
            kind,
            children,
            span,
            literal,
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
            }
        })
        .collect()
}

fn is_static_initializer(expr: &Expr) -> bool {
    if const_eval::eval(expr).is_ok() {
        return true;
    }
    match expr {
        Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::Char { .. }
        | Expr::String { .. }
        | Expr::LabelAddr { .. } => true,
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
        // &ident / &arr[const] / &ident.field...: 链接期可重定位常量
        Expr::AddrOf { e, .. } => match e.as_ref() {
            Expr::Ident { .. } | Expr::String { .. } => true,
            Expr::Index { arr, idx, .. } => {
                // &arr[i] / &ident.field[i]...: 链接期可重定位常量
                let base_ok = match arr.as_ref() {
                    Expr::Ident { .. } => true,
                    Expr::Member { expr, .. } => is_addr_of_member_ok(expr),
                    _ => false,
                };
                base_ok && const_eval::eval(idx).is_ok()
            }
            // &ident.field / &ident.field.field... : 链接期可重定位常量
            Expr::Member { expr, .. } => is_addr_of_member_ok(expr),
            _ => false,
        },
        // 数组成员隐式数组退化: init_uts_ns.name.machine (char[65]) 作为
        // 初始化器值等价 &init_uts_ns.name.machine[0], 是链接期可重定位
        // 地址常量 (与显式 & 形式一致)
        e @ Expr::Member { expr, .. } if matches!(e.ty(), Type::Array(..)) => {
            is_addr_of_member_ok(expr)
        }
        // 数组到指针衰减: arr + const / &arr[i] 的等价形式
        Expr::Binop {
            op: BinOp::Add,
            l,
            r,
            ..
        } => {
            (matches!(l.as_ref(), Expr::Ident { .. }) && const_eval::eval(r).is_ok())
                || (matches!(r.as_ref(), Expr::Ident { .. }) && const_eval::eval(l).is_ok())
        }
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
                            }
                            // 链式 designator: 全部 Index 都要能常量求值, Field 始终 OK
                            ast::Designator::Chain(items) => items.iter().all(|d| match d {
                                ast::Designator::Field(_) => true,
                                ast::Designator::Index(e) => const_eval::eval(e).is_ok(),
                                ast::Designator::Range(f, l) => {
                                    const_eval::eval(f).is_ok() && const_eval::eval(l).is_ok()
                                }
                                ast::Designator::Chain(_) => false,
                            }),
                        };
                        index_ok && is_static_initializer(value)
                    }
                }
            })
        }
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
        _ => false,
    }
}

fn expr_span(expr: &Expr) -> Span {
    match expr {
        Expr::Int { span, .. }
        | Expr::Float { span, .. }
        | Expr::ComplexLit { span, .. }
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
            assert!(matches!(v.ty, Type::Struct(ref n, _) if n == "static_key_false"));
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
    fn vector_bitwise_or_reports_e1007() {
        let src = b"typedef float v4f __attribute__((vector_size(16)));\n\
                     int main(void) { v4f a, b; return (int)(a | b)[0]; }\n";
        let (program, _) = crate::parser::parse(src, "e1007.c");
        let (module, _) = lower(program).expect("lower despite vector or");
        let err = validate(&module).expect_err("vector or must fail validation");
        assert!(err.iter().any(|d| d.code == "E1007"), "{err:?}");
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
        assert_eq!(
            module.functions[0].body[0].expressions[0].literal,
            Some(ExprLiteral::Int(5))
        );
    }
}
