// src/ast.rs - 抽象语法树
// 第一性原理: AST = 程序的"结构骨架",所有后续阶段(语义/优化/codegen)都基于它。
// suckless 准则: 节点种类尽量少;一个节点表示"一个语义概念",不重复定义。
// 标识符 Span 完整保留——错误信息、debug 都需要它。

use crate::diagnostics::Span;
use crate::tokens::Tok;

// Ident: 标识符,含名字+位置。Clone 是显式语义(代码里我们会显式 clone)
#[derive(Debug, Clone)]
pub struct Ident { pub name: String, pub span: Span }

#[derive(Debug, Clone)]
pub struct Program {
    pub decls: Vec<Decl>,
}

#[derive(Debug, Clone)]
pub enum Decl {
    Var(VarDecl),
    Func(FuncDecl),
    // struct/union/enum/typedef M2 再加
}

#[derive(Debug, Clone)]
pub struct VarDecl {
    pub ty:    TypeRef,
    pub name:  Ident,
    pub init:  Option<Expr>,
    pub span:  Span,
    pub is_extern: bool,
    pub is_static: bool,
}

#[derive(Debug, Clone)]
pub struct FuncDecl {
    pub ret_ty: TypeRef,
    pub name:   Ident,
    pub params: Vec<Param>,
    pub body:   Option<Block>,
    pub span:   Span,
    pub is_extern: bool,
    pub is_static: bool,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub ty:   TypeRef,
    pub name: Option<Ident>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Block {
    pub items: Vec<BlockItem>,
    pub span:  Span,
}

#[derive(Debug, Clone)]
pub enum BlockItem {
    Stmt(Stmt),
    Decl(Decl),
}

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
    // M2+ 还要: switch/case/default/compound
}

#[derive(Debug, Clone)]
pub enum Expr {
    Int    { v: i128, span: Span, ty: TypeRef },        // ty 填好方便 codegen
    Ident  { name: Ident, span: Span, ty: TypeRef },
    Binop  { op: BinOp, l: Box<Expr>, r: Box<Expr>, span: Span, ty: TypeRef },
    Unary  { op: UnOp,  e: Box<Expr>, span: Span, ty: TypeRef },
    Call   { callee: Ident, args: Vec<Expr>, span: Span, ty: TypeRef },
    Assign { op: BinOp, l: Box<Expr>, r: Box<Expr>, span: Span, ty: TypeRef },
    Cond   { cond: Box<Expr>, then_: Box<Expr>, else_: Box<Expr>, span: Span, ty: TypeRef },
    Cast   { ty: TypeRef, e: Box<Expr>, span: Span, ty2: TypeRef },
    Sizeof { ty: TypeRef, span: Span, ty2: TypeRef },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp { Add, Sub, Mul, Div, Mod,
                 And, Or, Xor, Shl, Shr,
                 Lt, Gt, Le, Ge, EqEq, Ne, AndAnd, OrOr }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp { Neg, Not, BitNot, Plus, Deref, AddrOf, PreInc, PreDec, PostInc, PostDec }

// 类型系统 - suckless: 内置类型即枚举 ID, 复杂类型按引用
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BaseTy {
    Void, Bool,
    Char, Schar, Uchar,         // char 默认按平台,我们用 Schar (匹配 x86_64 Linux)
    Short, Ushort,
    Int, Uint,
    Long, Ulong,
    LongLong, UlongLong,
    Float, Double, LongDouble,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TypeRef(pub u32);    // 指向 types 表

pub fn int_t()    -> TypeRef { TypeRef(BaseTy::Int as u32) }
pub fn long_t()   -> TypeRef { TypeRef(BaseTy::Long as u32) }
pub fn void_t()   -> TypeRef { TypeRef(BaseTy::Void as u32) }
pub fn char_t()   -> TypeRef { TypeRef(BaseTy::Char as u32) }
pub fn double_t() -> TypeRef { TypeRef(BaseTy::Double as u32) }

// 把原始 token 转为 BinOp/UnOp (Parser 内部用)
pub fn tok_to_binop(t: &Tok) -> Option<BinOp> {
    use crate::tokens::TokKind::*;
    Some(match t.kind {
        Plus  => BinOp::Add,  Minus => BinOp::Sub,  Star => BinOp::Mul,
        Slash => BinOp::Div,  Percent=> BinOp::Mod,
        Amp   => BinOp::And,  Pipe  => BinOp::Or,   Caret=> BinOp::Xor,
        Shl   => BinOp::Shl,  Shr   => BinOp::Shr,
        Lt    => BinOp::Lt,   Gt    => BinOp::Gt,   Le   => BinOp::Le,
        Ge    => BinOp::Ge,   EqEq  => BinOp::EqEq, Ne   => BinOp::Ne,
        AndAnd=> BinOp::AndAnd, OrOr=> BinOp::OrOr,
        _ => return None,
    })
}
