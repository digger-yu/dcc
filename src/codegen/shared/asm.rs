// src/codegen/shared/asm.rs - 汇编事件记录器
// Phase C: 数据驱动代码生成架构重构
//
// 第一性原理：汇编指令生成的逻辑在所有 ISA 后端中相同（编码规则不同）
// 目标：提取通用汇编构建逻辑

use std::fmt;

/// Assembly instruction encoding format
#[derive(Debug, Clone)]
#[derive(Default)]
pub enum EncodingFormat {
    /// Intel syntax (AT&T alternative available)
    #[default]
    Intel,
    /// AT&T syntax (x86 default)
    Att,
}


/// Assembly operand types
#[derive(Debug, Clone)]
pub enum Operand {
    /// Register reference
    Register(String, bool), // name, is_directive
    /// Immediate value
    Immediate(i64),
    /// Memory access: [base + index*scale + offset]
    Memory {
        base: Option<String>,
        index: Option<String>,
        scale: u32,
        offset: i64,
    },
    /// Label reference
    Label(String),
    /// Symbol with displacement
    SymbolOffset { symbol: String, offset: i64 },
    /// Floating point immediate
    Float(f64),
    /// Custom expression
    Expression(Box<Expression>),
}

/// Simple expression type for memory addressing
#[derive(Debug, Clone)]
pub enum Expression {
    BinaryOp {
        op: &'static str,
        left: Box<Expression>,
        right: Box<Expression>,
    },
    UnaryOp {
        op: &'static str,
        expr: Box<Expression>,
    },
    Identifier(String),
    Literal(i64),
}

/// Instruction mnemonic
#[derive(Debug, Clone)]
pub struct Instruction {
    pub mnemonic: String,
    pub operands: Vec<Operand>,
    pub attrs: InstructionAttributes,
}

/// Instruction attributes for special handling
#[derive(Debug, Default, Clone)]
pub struct InstructionAttributes {
    pub is_nop: bool,
    pub is_barrier: bool,
    pub is_branch: bool,
    pub is_call: bool,
    pub is_return: bool,
    pub predicts_taken: bool,
    pub depends_on_flags: bool,
}

impl Instruction {
    pub fn new(mnemonic: &str) -> Self {
        Self {
            mnemonic: mnemonic.to_string(),
            operands: Vec::new(),
            attrs: InstructionAttributes::default(),
        }
    }

    pub fn with_operands(mut self, operands: Vec<Operand>) -> Self {
        self.operands = operands;
        self
    }

    pub fn set_attrs(&mut self, attrs: InstructionAttributes) {
        self.attrs = attrs;
    }

    /// Format instruction as assembly text
    pub fn fmt(&self, format: &EncodingFormat) -> String {
        match format {
            EncodingFormat::Att => self.fmt_att(),
            EncodingFormat::Intel => self.fmt_intel(),
        }
    }

    fn fmt_att(&self) -> String {
        // AT&T syntax: mnemonic src, dst
        let mut result = self.mnemonic.clone();

        for operand in &self.operands {
            result.push_str(", ");
            result.push_str(&operand.fmt_att());
        }

        result
    }

    fn fmt_intel(&self) -> String {
        // Intel syntax: dst, src
        let mut result = self.mnemonic.clone();

        for (idx, operand) in self.operands.iter().enumerate() {
            if idx > 0 {
                result.push_str(", ");
            }
            result.push_str(&operand.fmt_intel());
        }

        result
    }
}

impl Operand {
    fn fmt_att(&self) -> String {
        match self {
            Operand::Register(name, _) => format!("%{}", name),
            Operand::Immediate(val) => format!("{}", val),
            Operand::Memory {
                base,
                index,
                scale,
                offset,
            } => {
                let mut result = String::new();

                if *offset != 0 {
                    result.push_str(&format!("{}", offset));
                }

                if let Some(b) = base {
                    result.push('[');
                    result.push('%');
                    result.push_str(b);

                    if let Some(i) = index {
                        if *scale != 0 {
                            result.push_str(",%");
                            result.push_str(i);
                            result.push_str(&format!("*{}", scale));
                        } else {
                            result.push_str(",%");
                            result.push_str(i);
                        }
                    }
                    result.push(']');
                }

                if result.is_empty() {
                    "0".to_string()
                } else {
                    result
                }
            },
            Operand::Label(label) => label.clone(),
            Operand::SymbolOffset { symbol, offset } => {
                if *offset > 0 {
                    format!("{}+{}", symbol, offset)
                } else if *offset < 0 {
                    format!("{}-{}", symbol, -offset)
                } else {
                    symbol.clone()
                }
            },
            Operand::Float(val) => format!("{}", val),
            Operand::Expression(expr) => expr.fmt(),
        }
    }

    fn fmt_intel(&self) -> String {
        match self {
            Operand::Register(name, _) => name.to_string(),
            Operand::Immediate(val) => format!("${}", val),
            Operand::Memory {
                base,
                index,
                scale,
                offset,
            } => {
                let mut inner = String::new();

                if *offset != 0 {
                    inner.push_str(&format!("{}", offset));
                }

                if let Some(b) = base {
                    inner.push('(');
                    inner.push_str(b);
                    inner.push(')');
                }

                if let Some(i) = index {
                    if !inner.is_empty() {
                        inner.push('+');
                    }
                    inner.push_str(i);

                    if *scale != 0 {
                        inner.push('*');
                        inner.push_str(&scale.to_string());
                    }
                }

                format!("[{}]", inner)
            },
            Operand::Label(label) => label.clone(),
            Operand::SymbolOffset { symbol, offset } => {
                if *offset > 0 {
                    format!("{}({})", symbol, offset)
                } else if *offset < 0 {
                    format!("{}({})", symbol, -offset)
                } else {
                    symbol.clone()
                }
            },
            Operand::Float(val) => format!("{}", val),
            Operand::Expression(expr) => expr.fmt(),
        }
    }
}

impl fmt::Display for Operand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.fmt_intel())
    }
}

impl fmt::Display for Expression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.fmt())
    }
}

impl Expression {
    fn fmt(&self) -> String {
        match self {
            Expression::BinaryOp { op, left, right } => {
                format!("({} {} {})", left, op, right)
            },
            Expression::UnaryOp { op, expr } => {
                format!("{}{}", op, expr)
            },
            Expression::Identifier(name) => name.clone(),
            Expression::Literal(val) => val.to_string(),
        }
    }
}

/// Assembly builder (ISA-independent construction logic)
pub struct AssemblyBuilder {
    instructions: Vec<Instruction>,
    labels: Vec<String>,
    directives: Vec<Directive>,
}

/// Assembly directives
#[derive(Debug, Clone)]
pub enum Directive {
    /// Section change
    Section(&'static str),
    /// Global symbol
    Global(&'static str),
    /// Local symbol
    Local(&'static str),
    /// Type attribute
    Type(&'static str, TypeAttribute),
    /// Size attribute
    Size(&'static str, usize),
    /// Alignment
    Align(u32),
    /// Space allocation
    Space(usize),
    /// Data definition
    Data(Vec<u8>),
    /// String definition
    String(String),
}

/// Type attributes for symbols
#[derive(Debug, Clone)]
pub enum TypeAttribute {
    Function,
    Object,
    File,
    Notype,
}

impl Default for AssemblyBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl AssemblyBuilder {
    pub fn new() -> Self {
        Self {
            instructions: Vec::new(),
            labels: Vec::new(),
            directives: Vec::new(),
        }
    }

    /// Add an instruction
    pub fn add_instruction(&mut self, instr: Instruction) {
        self.instructions.push(instr);
    }

    /// Define a label
    pub fn label(&mut self, name: &str) {
        self.labels.push(name.to_string());
    }

    /// Add a directive
    pub fn directive(&mut self, dir: Directive) {
        self.directives.push(dir);
    }

    /// Generate function prologue (ISA-specific implementation needed)
    pub fn prologue(&mut self, _frame_size: usize) {
        // This will be implemented per ISA
        // Common pattern: push rbp; mov rbp, rsp
    }

    /// Generate function epilogue (ISA-specific implementation needed)
    pub fn epilogue(&mut self) {
        // Common pattern: pop rbp; ret
    }

    /// Emit assembly text
    pub fn emit_text(&self, format: EncodingFormat) -> String {
        let mut text = String::new();

        // Directives
        for dir in &self.directives {
            text.push_str(&dir.fmt(&format));
            text.push('\n');
        }

        // Labels
        for label in &self.labels {
            text.push_str(label);
            text.push(':');
            text.push('\n');
        }

        // Instructions
        for instr in &self.instructions {
            text.push('\t');
            text.push_str(&instr.fmt(&format));
            text.push('\n');
        }

        text
    }
}

impl Directive {
    fn fmt(&self, _format: &EncodingFormat) -> String {
        // For now, use generic assembly notation
        match self {
            Directive::Section(name) => format!(".section {}", name),
            Directive::Global(name) => format!(".globa {}", name),
            Directive::Local(name) => format!(".local {}", name),
            Directive::Type(name, _) => format!(".type {},@function", name),
            Directive::Size(name, size) => format!(".size {},{}", name, size),
            Directive::Align(n) => format!(".align {}", n),
            Directive::Space(size) => format!(".space {}", size),
            Directive::Data(data) => {
                let hex: String = data.iter().map(|b| format!("{:02x}", b)).collect();
                format!(".string \"{}\"", hex)
            },
            Directive::String(s) => format!(".string \"{}\"", s),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_instruction() {
        let instr = Instruction::new("mov").with_operands(vec![
            Operand::Register("RAX".to_string(), false),
            Operand::Immediate(42),
        ]);

        assert_eq!(instr.mnemonic, "mov");
        assert_eq!(instr.operands.len(), 2);
    }

    #[test]
    fn test_memory_operand() {
        let mem = Operand::Memory {
            base: Some("RBP".to_string()),
            index: None,
            scale: 0,
            offset: -8,
        };

        assert_eq!(mem.fmt_intel(), "[-8(RBP)]");
    }

    #[test]
    fn test_assembly_builder() {
        let mut builder = AssemblyBuilder::new();

        builder.label("my_function");
        builder.add_instruction(
            Instruction::new("push")
                .with_operands(vec![Operand::Register("RBX".to_string(), false)]),
        );

        let text = builder.emit_text(EncodingFormat::Intel);
        assert!(text.contains("my_function"));
        assert!(text.contains("push"));
    }
}
