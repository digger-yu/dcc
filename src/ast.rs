// src/ast.rs - 抽象语法树
// 第一性原理: AST = 程序的"结构骨架"。类型系统必须能表达 C 的全部类型。
// suckless 准则: 节点种类尽量少;一个节点表示"一个语义概念"。

use crate::diagnostics::Span;

#[derive(Debug, Clone)]
pub struct Ident { pub name: String, pub span: Span }

#[derive(Debug, Clone)]
pub struct Program { pub decls: Vec<Decl> }

#[derive(Debug, Clone)]
pub enum Decl {
    Var(VarDecl),
    Func(FuncDecl),
    StructDef(StructDef),
    UnionDef(StructDef),
    EnumDef(EnumDef),
    Typedef { name: Ident, ty: Type, span: Span },
}

#[derive(Debug, Clone)]
pub struct VarDecl {
    pub ty: Type, pub name: Ident, pub init: Option<Expr>,
    pub span: Span, pub is_extern: bool, pub is_static: bool,
}

#[derive(Debug, Clone)]
pub struct FuncDecl {
    pub ret_ty: Type, pub name: Ident, pub params: Vec<Param>,
    pub body: Option<Block>, pub span: Span,
    pub is_extern: bool, pub is_static: bool,
}

#[derive(Debug, Clone)]
pub struct Param { pub ty: Type, pub name: Option<Ident>, pub span: Span }

#[derive(Debug, Clone)]
pub struct Block { pub items: Vec<BlockItem>, pub span: Span }

#[derive(Debug, Clone)]
pub enum BlockItem { Stmt(Stmt), Decl(Decl) }

#[derive(Debug, Clone)]
pub enum Stmt {
    Return { val: Option<Expr>, span: Span },
    Expr    { e: Expr, span: Span },
    If      { cond: Expr, then_: Box<Stmt>, else_: Option<Box<Stmt>>, span: Span },
    While   { cond: Expr, body: Box<Stmt>, span: Span },
    Do      { body: Box<Stmt>, cond: Expr, span: Span },
    For     { init: Option<Box<Stmt>>, cond: Option<Expr>,
              step: Option<Expr>, body: Box<Stmt>, span: Span },
    Block   { b: Block, span: Span },
    Break   { span: Span },
    Continue{ span: Span },
    Goto    { target: Ident, span: Span },
    Label   { name: Ident, stmt: Box<Stmt>, span: Span },
    Switch  { expr: Expr, cases: Vec<Case>, span: Span },
}

#[derive(Debug, Clone)]
pub struct Case {
    pub val: Option<Expr>,
    pub body: Block,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Int       { v: i128, span: Span, ty: Type },
    String    { data: Vec<u8>, span: Span, ty: Type },
    Char      { v: u8, span: Span, ty: Type },
    Ident     { name: Ident, span: Span, ty: Type },
    Binop     { op: BinOp, l: Box<Expr>, r: Box<Expr>, span: Span, ty: Type },
    Unary     { op: UnOp,  e: Box<Expr>, span: Span, ty: Type },
    Call      { callee: Ident, args: Vec<Expr>, span: Span, ty: Type },
    Assign    { op: BinOp, l: Box<Expr>, r: Box<Expr>, span: Span, ty: Type },
    Cond      { cond: Box<Expr>, then_: Box<Expr>, else_: Box<Expr>, span: Span, ty: Type },
    Cast      { ty: Type, e: Box<Expr>, span: Span },
    SizeofTy  { ty: Type, span: Span },
    SizeofExpr{ e: Box<Expr>, span: Span },
    Index     { arr: Box<Expr>, idx: Box<Expr>, span: Span, ty: Type },
    Member    { expr: Box<Expr>, field: Ident, span: Span, ty: Type, is_arrow: bool },
    AddrOf    { e: Box<Expr>, span: Span, ty: Type },
    Deref     { e: Box<Expr>, span: Span, ty: Type },
    PostInc   { e: Box<Expr>, span: Span, ty: Type },
    PostDec   { e: Box<Expr>, span: Span, ty: Type },
    PreInc    { e: Box<Expr>, span: Span, ty: Type },
    PreDec    { e: Box<Expr>, span: Span, ty: Type },
    CompoundLit { ty: Type, inits: Vec<Expr>, span: Span },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add, Sub, Mul, Div, Mod,
    And, Or, Xor, Shl, Shr,
    Lt, Gt, Le, Ge, EqEq, Ne, AndAnd, OrOr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp { Neg, Not, BitNot, Plus }

// ===== 类型系统 =====
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    Void, Bool,
    Char, SChar, UChar,
    Short, UShort,
    Int, UInt,
    Long, ULong,
    LongLong, ULongLong,
    Float, Double, LongDouble,
    Ptr(Box<Type>),
    Array(Box<Type>, usize),
    Struct(String, Vec<Field>),
    Union(String, Vec<Field>),
    Enum(String, Vec<(String, i64)>),
    Func(Box<Type>, Vec<Type>),
    Named(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub ty: Type,
    pub offset: usize,
    pub bit_width: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct StructDef {
    pub name: Ident, pub fields: Vec<Field>,
    pub is_union: bool, pub span: Span,
}

#[derive(Debug, Clone)]
pub struct EnumDef {
    pub name: Ident, pub variants: Vec<(Ident, Option<i64>)>, pub span: Span,
}

impl Type {
    pub fn size(&self) -> usize {
        match self {
            Type::Void => 0,
            Type::Bool | Type::Char | Type::SChar | Type::UChar => 1,
            Type::Short | Type::UShort => 2,
            Type::Int | Type::UInt | Type::Float | Type::Enum(_, _) => 4,
            Type::Long | Type::ULong | Type::Ptr(_) | Type::Array(_, _)
            | Type::LongLong | Type::ULongLong | Type::Double => 8,
            Type::LongDouble => 16,
            Type::Struct(_, fields) => {
                fields.last().map(|f| f.offset + f.ty.size()).unwrap_or(0)
            }
            Type::Union(_, fields) => {
                fields.iter().map(|f| f.ty.size()).max().unwrap_or(0)
            }
            _ => 8,
        }
    }

    pub fn align(&self) -> usize {
        match self {
            Type::Void => 1,
            Type::Bool | Type::Char | Type::SChar | Type::UChar => 1,
            Type::Short | Type::UShort => 2,
            Type::Int | Type::UInt | Type::Float | Type::Enum(_, _) => 4,
            Type::Long | Type::ULong | Type::Ptr(_) | Type::Array(_, _)
            | Type::LongLong | Type::ULongLong | Type::Double => 8,
            Type::LongDouble => 16,
            Type::Struct(_, fields) => fields.iter().map(|f| f.ty.align()).max().unwrap_or(1),
            Type::Union(_, fields) => fields.iter().map(|f| f.ty.align()).max().unwrap_or(1),
            _ => 8,
        }
    }

    pub fn is_ptr(&self) -> bool { matches!(self, Type::Ptr(_)) }
    pub fn is_int(&self) -> bool {
        matches!(self, Type::Bool | Type::Char | Type::SChar | Type::UChar
            | Type::Short | Type::UShort | Type::Int | Type::UInt
            | Type::Long | Type::ULong | Type::LongLong | Type::ULongLong)
    }
    pub fn is_arith(&self) -> bool { self.is_int() || matches!(self, Type::Float | Type::Double | Type::LongDouble) }

    pub fn layout_struct(fields: &mut Vec<Field>, is_union: bool) {
        if is_union {
            for f in fields.iter_mut() { f.offset = 0; }
        } else {
            let mut off = 0usize;
            for f in fields.iter_mut() {
                let a = f.ty.align();
                off = (off + a - 1) & !(a - 1);
                f.offset = off;
                off += f.ty.size();
            }
        }
    }
}

pub fn int_t()  -> Type { Type::Int }
pub fn long_t() -> Type { Type::Long }
pub fn void_t() -> Type { Type::Void }
pub fn char_t() -> Type { Type::Char }

impl Expr {
    pub fn ty(&self) -> Type {
        match self {
            Expr::Int { ty, .. } | Expr::String { ty, .. } | Expr::Char { ty, .. }
            | Expr::Ident { ty, .. } | Expr::Binop { ty, .. } | Expr::Unary { ty, .. }
            | Expr::Call { ty, .. } | Expr::Assign { ty, .. } | Expr::Cond { ty, .. }
            | Expr::Index { ty, .. } | Expr::Member { ty, .. }
            | Expr::AddrOf { ty, .. } | Expr::Deref { ty, .. }
            | Expr::PostInc { ty, .. } | Expr::PostDec { ty, .. }
            | Expr::PreInc { ty, .. } | Expr::PreDec { ty, .. } => ty.clone(),
            Expr::Cast { ty, .. } => ty.clone(),
            Expr::SizeofTy { .. } | Expr::SizeofExpr { .. } | Expr::CompoundLit { .. } => Type::Int,
        }
    }
}
