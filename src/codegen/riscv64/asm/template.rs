// src/codegen/riscv64/asm/template.rs - Assembly Template Expansion Engine
// Phase B Day 3 - Extracted from riscv64.rs.bak (2026/9/22)
//
// Source location in riscv64.rs.bak:
// - Inline asm templates use $N placeholders for operand substitution
// - Modifiers like w/h/b modify register width selection

/// RISC-V assembly template string handler
pub struct AsmTemplateHandler;

impl AsmTemplateHandler {
    /// Expand a template with operand substitutions
    /// 
    /// Supports:
    /// - $N - Nth operand (0-indexed or 1-indexed depending on context)
    /// - ${name} - Named operands
    /// - %{modifier}% - Width modifiers (w, h, b)
    pub fn expand(template: &str, operands: &[Operand]) -> String {
        let mut result = String::new();
        let mut chars = template.chars().peekable();

        while let Some(c) = chars.next() {
            if c == '$' {
                // Parse placeholder
                if let Some(placeholder) = Self::parse_placeholder(&mut chars) {
                    if let Some(replacement) = Self::resolve_operand(&placeholder, operands) {
                        result.push_str(&replacement);
                    } else {
                        result.push('$');
                        result.push_str(&placeholder);
                    }
                } else {
                    result.push('$');
                }
            } else if c == '%' {
                // Modifier handling (width specifiers)
                if let Some(modifier) = chars.peek() {
                    if matches!(modifier, 'w' | 'h' | 'b') {
                        let mod_char = chars.next().unwrap();
                        // Apply modifier to previous operand reference
                        // (simplified - real implementation tracks context)
                        result.push('%');
                        result.push(mod_char);
                    } else {
                        result.push('%');
                    }
                } else {
                    result.push('%');
                }
            } else {
                result.push(c);
            }
        }

        result
    }

    /// Parse a placeholder starting after '$'
    fn parse_placeholder(chars: &mut std::iter::Peekable<std::str::Chars>) -> Option<String> {
        let mut name = String::new();
        
        loop {
            match chars.peek() {
                Some(c) if c.is_alphanumeric() || *c == '_' => {
                    name.push(chars.next().unwrap());
                }
                Some('{') => {
                    chars.next(); // consume '{'
                    // Parse named placeholder until '}'
                    loop {
                        match chars.peek() {
                            Some('}') => {
                                chars.next();
                                return Some(name);
                            }
                            Some(c) => {
                                name.push(chars.next().unwrap());
                            }
                            None => break,
                        }
                    }
                    return Some(name);
                }
                _ => break,
            }
        }

        if !name.is_empty() {
            Some(name)
        } else {
            None
        }
    }

    /// Resolve a placeholder name to actual value
    fn resolve_operand(placeholder: &str, operands: &[Operand]) -> Option<String> {
        // Numeric operand ($0, $1, etc.)
        if let Ok(n) = placeholder.parse::<usize>() {
            operands.get(n).map(|op| op.to_string())
        }
        // Named operand (${name} format)
        else {
            operands.iter()
                .find(|op| op.name == placeholder)
                .map(|op| op.to_string())
        }
    }
}

/// Operand representation for template expansion
#[derive(Debug, Clone)]
pub struct Operand {
    pub name: String,
    pub value: OperandValue,
    pub constraint: ConstraintInfo,
    pub reg_name: Option<String>,
}

impl Operand {
    pub fn new(name: &str, value: OperandValue) -> Self {
        Self {
            name: name.to_string(),
            value,
            constraint: ConstraintInfo::default(),
            reg_name: None,
        }
    }

    pub fn as_register(&self) -> Option<&str> {
        self.reg_name.as_deref()
    }

    pub fn as_immediate(&self) -> Option<i64> {
        match &self.value {
            OperandValue::Immediate(v) => Some(*v),
            _ => None,
        }
    }

    pub fn is_memory(&self) -> bool {
        matches!(self.value, OperandValue::Memory(_))
    }
}

impl ToString for Operand {
    fn to_string(&self) -> String {
        if let Some(reg) = &self.reg_name {
            reg.clone()
        } else {
            match &self.value {
                OperandValue::Immediate(v) => v.to_string(),
                OperandValue::Register(r) => r.clone(),
                OperandValue::Memory(m) => m.clone(),
                OperandValue::Symbol(s) => s.clone(),
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum OperandValue {
    Immediate(i64),
    Register(String),
    Memory(String),
    Symbol(String),
}

#[derive(Debug, Clone, Default)]
pub struct ConstraintInfo {
    pub rw: RwConstraint,
    pub typ: OperandType,
    pub modifier: char, // w=half, h=half, b=byte
}

#[derive(Debug, Clone, Copy, Default)]
pub enum RwConstraint {
    #[default]
    Read,
    Write,
    ReadWrite,
}

#[derive(Debug, Clone, Copy, Default)]
pub enum OperandType {
    #[default]
    Integer,
    Float,
    Memory,
}

// ========================================================================
// Template Builder for programmatic assembly generation
// ========================================================================

/// Fluent API for building assembly templates programmatically
pub struct TemplateBuilder {
    template: String,
    params: Vec<(String, OperandType)>,
}

impl TemplateBuilder {
    pub fn new() -> Self {
        Self {
            template: String::new(),
            params: Vec::new(),
        }
    }

    pub fn add_param(&mut self, name: &str, typ: OperandType) -> &mut Self {
        self.params.push((name.to_string(), typ));
        self
    }

    pub fn append(&mut self, text: &str) -> &mut Self {
        self.template.push_str(text);
        self
    }

    pub fn param(&mut self, name: &str) -> &mut Self {
        self.template.push('$');
        self.template.push_str(name);
        self
    }

    pub fn build(self) -> String {
        self.template
    }

    /// Build and get the final template string
    pub fn finalize(self) -> (String, Vec<(String, OperandType)>) {
        (self.template, self.params)
    }
}

impl Default for TemplateBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ========================================================================
// Instruction Templates Library
// ========================================================================

/// Common RISC-V instruction templates
pub trait InstructionTemplates {
    fn jalr_template() -> String;
    fn addi_template() -> String;
    fn lw_template() -> String;
    fn sw_template() -> String;
    fn jr_template() -> String;
    fn ret_template() -> String;
    fn call_template() -> String;
}

impl InstructionTemplates for () {
    /// JALR: jalr rd, rs1, imm
    fn jalr_template() -> String {
        "jalr $rd, $rs1, $imm".to_string()
    }

    /// ADDI: addi rd, rs1, imm  
    fn addi_template() -> String {
        "addi $rd, $rs1, $imm".to_string()
    }

    /// LW: lw rd, offset(rs1)
    fn lw_template() -> String {
        "lw $rd, $offset($rs1)".to_string()
    }

    /// SW: sw rs2, offset(rs1)
    fn sw_template() -> String {
        "sw $rs2, $offset($rs1)".to_string()
    }

    /// Jump register: jr rs1
    fn jr_template() -> String {
        "jr $rs1".to_string()
    }

    /// Return: ret (= jalr x0, ra, 0)
    fn ret_template() -> String {
        "ret".to_string()
    }

    /// Call external function: call symbol
    fn call_template() -> String {
        "call $sym".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_placeholder() {
        let operands = vec![
            Operand::new("a0", OperandValue::Immediate(10)),
            Operand::new("a1", OperandValue::Immediate(20)),
        ];

        let result = AsmTemplateHandler::expand("$0 + $1", &operands);
        assert_eq!(result, "10 + 20");
    }

    #[test]
    fn test_named_placeholder() {
        let operand = Operand::new("val", OperandValue::Register("x1".to_string()));
        let operands = vec![operand];

        let result = AsmTemplateHandler::expand("$val", &operands);
        assert_eq!(result, "x1");
    }

    #[test]
    fn test_instruction_templates() {
        let t = InstructionTemplates::jalr_template();
        assert!(t.contains("$rd"));
        
        let t = InstructionTemplates::lw_template();
        assert!(t.contains("$offset"));
    }

    #[test]
    fn test_template_builder() {
        let mut builder = TemplateBuilder::new();
        builder.append("addi $rd, $rs1, ")
               .param("imm")
               .append("\n");
        
        let (template, _) = builder.finalize();
        assert!(template.contains("$rd"));
        assert!(template.contains("$imm"));
    }
}
