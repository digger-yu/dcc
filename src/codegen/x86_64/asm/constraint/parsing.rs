// Constraint parsing utilities for inline assembly
// Contains: parse constraints, match patterns, validate operands

use crate::ast::Expr;

/// Parse a single asm constraint character/digit
pub fn parse_constraint_char(ch: char) -> Option<ConstraintKind> {
    match ch {
        'i' | 'I' => Some(ConstraintKind::Immediate),
        'n' | 'N' => Some(ConstraintKind::Number),
        'e' | 'E' => Some(ConstraintKind::RegisterExt),
        'r' | 'R' => Some(ConstraintKind::GeneralReg),
        'x' | 'X' => Some(ConstraintKind::XmmReg),
        't' | 'T' => Some(ConstraintKind::X87Reg),
        'y' | 'Y' => Some(ConstraintKind::MmxReg),
        '=' => Some(ConstraintKind::Output),
        '+' => Some(ConstraintKind::InOut),
        '&' => Some(ConstraintKind::EarlyClobber),
        '%' => Some(ConstraintKind::PercentModifier),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConstraintKind {
    Immediate,
    Number,
    RegisterExt,
    GeneralReg,
    XmmReg,
    X87Reg,
    MmxReg,
    Output,
    InOut,
    EarlyClobber,
    PercentModifier,
}
