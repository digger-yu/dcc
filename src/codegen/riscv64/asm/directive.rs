// src/codegen/riscv64/asm/directive.rs - Assembly Directive Emitter
// Phase B Day 3 - Extracted from riscv64.rs.bak (2026/9/22)
//
// Source location in riscv64.rs.bak:
// - Global and function directives are handled during code generation (~line 350+)
// - .cfi_, .align, .globl, .type, .size, .word, .quad directives

use std::collections::HashMap;

/// RISC-V assembly directive emitter
pub struct AsmDirectiveEmitter {
    /// Current section name
    pub current_section: String,
    /// Alignment state for .align/.p2align
    pub alignment: Option<u32>,
    /// Pending bytes to emit after alignment
    pub pending_align_bytes: Vec<u8>,
}

impl Default for AsmDirectiveEmitter {
    fn default() -> Self {
        Self::new()
    }
}

impl AsmDirectiveEmitter {
    pub fn new() -> Self {
        Self {
            current_section: ".text".to_string(),
            alignment: None,
            pending_align_bytes: Vec::new(),
        }
    }

    // ========================================================================
    // Data Definition Directives
    // ========================================================================

    /// Emit .word directive (32-bit value)
    pub fn emit_word(&mut self, value: u32, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&(value as i32).to_le_bytes());
    }

    /// Emit .half directive (16-bit value)
    pub fn emit_half(&mut self, value: u16, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&value.to_le_bytes());
    }

    /// Emit .byte directive (8-bit value)
    pub fn emit_byte(&mut self, value: u8, buf: &mut Vec<u8>) {
        buf.push(value);
    }

    /// Emit .quad directive (64-bit value)
    pub fn emit_quad(&mut self, value: u64, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&value.to_le_bytes());
    }

    /// Emit multi-byte sequence
    pub fn emit_bytes(&mut self, values: &[u8], buf: &mut Vec<u8>) {
        for v in values {
            self.emit_byte(*v, buf);
        }
    }

    // ========================================================================
    // Alignment Directives
    // ========================================================================

    /// Parse .align directive (power of 2 alignment)
    pub fn parse_align_directive(&mut self, operand: &str) -> Option<u32> {
        operand.trim().parse::<u32>().ok()
    }

    /// Parse .p2align directive (same as .align but more explicit)
    pub fn parse_p2align_directive(&mut self, operand: &str) -> Option<u32> {
        self.parse_align_directive(operand)
    }

    /// Align buffer to specified boundary, fill with NOPs
    pub fn apply_alignment(&mut self, align_exp: u32, buf: &mut Vec<u8>) {
        if align_exp == 0 {
            return;
        }

        let target_alignment = 1u32 << align_exp;
        let current_pos = buf.len();
        
        if current_pos % target_alignment == 0 {
            return;
        }

        let padding_needed = target_alignment - (current_pos % target_alignment) as u32;
        let nop = [0x13]; // addi x0, x0, 0
        
        for _ in 0..padding_needed {
            buf.extend_from_slice(&nop);
        }
    }

    // ========================================================================
    // Symbol Directives
    // ========================================================================

    /// Emit .globl directive mark (export symbol)
    pub fn emit_globl(&mut self, symbol_name: &str, symbols: &mut Vec<String>) {
        if !symbols.contains(&symbol_name.to_string()) {
            symbols.push(symbol_name.to_string());
        }
    }

    /// Emit .type directive (set symbol type: function/object)
    pub fn emit_type(&mut self, symbol_name: &str, sym_type: &str) -> String {
        format!(".type {}, {}", symbol_name, sym_type)
    }

    /// Emit .size directive (set symbol size)
    pub fn emit_size(&mut self, symbol_name: &str, size: u64) -> String {
        format!(".size {}, {}", symbol_name, size)
    }

    // ========================================================================
    // Debug Info Directives (.cfi_*)
    // ========================================================================

    /// Emit .cfi_startproc (begin function prologue info)
    pub fn emit_cfi_startproc(&self) -> String {
        ".cfi_startproc".to_string()
    }

    /// Emit .cfi_endproc (end function prologue info)
    pub fn emit_cfi_endproc(&self) -> String {
        ".cfi_endproc".to_string()
    }

    /// Emit .cfi_def_cfa (define CFA - Call Frame Address)
    pub fn emit_cfi_def_cfa(&self, reg: &str, offset: u64) -> String {
        format!(".cfi_def_cfa {}, {}", reg, offset)
    }

    /// Emit .cfi_def_cfa_register
    pub fn emit_cfi_def_cfa_register(&self, reg: u8) -> String {
        format!(".cfi_def_cfa_register {}", reg)
    }

    /// Emit .cfi_def_cfa_offset
    pub fn emit_cfi_def_cfa_offset(&self, offset: u64) -> String {
        format!(".cfi_def_cfa_offset {}", offset)
    }

    /// Emit .cfi_rel_offset (register saved at frame offset)
    pub fn emit_cfi_rel_offset(&self, reg: u8, offset: i64) -> String {
        format!(".cfi_rel_offset {}, {}", reg, offset)
    }

    /// Emit .cfi_adjust_cfa_offset
    pub fn emit_cfi_adjust_cfa_offset(&self, delta: i64) -> String {
        if delta >= 0 {
            format!(".cfi_adjust_cfa_offset {}", delta)
        } else {
            format!(".cfi_adjust_cfa_offset {}", -delta)
        }
    }

    /// Emit .cfi_escape (generic escape for complex unwind info)
    pub fn emit_cfi_escape(&self, data: &str) -> String {
        format!(".cfi_escape {}", data)
    }

    // ========================================================================
    // Section Directives
    // ========================================================================

    /// Change current section
    pub fn set_section(&mut self, section_name: &str) {
        self.current_section = section_name.to_string();
    }

    /// Get current section name
    pub fn get_current_section(&self) -> &str {
        &self.current_section
    }

    /// Check if we're in text section
    pub fn is_text_section(&self) -> bool {
        self.current_section == ".text"
    }

    /// Check if we're in rodata section
    pub fn is_rodata_section(&self) -> bool {
        self.current_section == ".rodata"
    }

    // ========================================================================
    // Generic Directive Emitter
    // ========================================================================

    /// Parse and emit an arbitrary directive
    pub fn emit_directive(name: &str, operands: &[String]) -> Option<String> {
        let mut result = String::from(".");
        result.push_str(name);

        if !operands.is_empty() {
            result.push(' ');
            result.push_str(&operands.join(","));
        }

        Some(result)
    }

    /// Handle common directives in one place
    pub fn handle_directive(name: &str, args: &[String], ctx: &mut DirectivesContext) -> String {
        match name {
            "align" | "p2align" => {
                if let Some(exp) = ctx.directive_emitter.parse_align_directive(&args[0]) {
                    let _ = ctx.directive_emitter.apply_alignment(exp, &mut ctx.buffer);
                    format!(".{} {}", name, args.join(" "))
                } else {
                    String::new()
                }
            }
            "globl" => {
                if let Some(sym) = args.first() {
                    ctx.directive_emitter.emit_globl(sym, &mut ctx.exported_symbols);
                }
                format!(".{} {}", name, args.join(" "))
            }
            "type" => {
                if let (Some(sym), Some(typ)) = (args.get(0), args.get(1)) {
                    format!(".type {}, {}", sym, typ)
                } else {
                    String::new()
                }
            }
            "size" => {
                if let (Some(sym), Some(size)) = (args.get(0), args.get(1)) {
                    format!(".size {}, {}", sym, size)
                } else {
                    String::new()
                }
            }
            "section" => {
                if let Some(sec) = args.first() {
                    ctx.directive_emitter.set_section(sec);
                }
                format!(".section {}", args.join(" "))
            }
            "word" => {
                if let Some(val) = args.first().and_then(|s| s.parse::<u32>().ok()) {
                    ctx.directive_emitter.emit_word(val, &mut ctx.buffer);
                    format!(".word {}", val)
                } else {
                    format!(".word {}", args.join(", "))
                }
            }
            "quad" => {
                if let Some(val) = args.first().and_then(|s| s.parse::<u64>().ok()) {
                    ctx.directive_emitter.emit_quad(val, &mut ctx.buffer);
                    format!(".quad {}", val)
                } else {
                    format!(".quad {}", args.join(", "))
                }
            }
            "half" => {
                if let Some(val) = args.first().and_then(|s| s.parse::<u16>().ok()) {
                    ctx.directive_emitter.emit_half(val, &mut ctx.buffer);
                    format!(".half {}", val)
                } else {
                    format!(".half {}", args.join(", "))
                }
            }
            "byte" => {
                for arg in args {
                    if let Some(val) = arg.parse::<u8>().ok() {
                        ctx.directive_emitter.emit_byte(val, &mut ctx.buffer);
                    }
                }
                format!(".byte {}", args.join(", "))
            }
            _ => Self::emit_directive(name, args).unwrap_or_default(),
        }
    }
}

/// Context for directive emission
pub struct DirectivesContext<'a> {
    pub directive_emitter: &'a mut AsmDirectiveEmitter,
    pub buffer: &'a mut Vec<u8>,
    pub exported_symbols: &'a mut Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_data_definitions() {
        let mut emitter = AsmDirectiveEmitter::new();
        let mut buf = Vec::new();

        emitter.emit_word(0x12345678, &mut buf);
        assert_eq!(buf.len(), 4);

        emitter.emit_half(0x1234, &mut buf);
        assert_eq!(buf.len(), 6);

        emitter.emit_byte(0xAB, &mut buf);
        assert_eq!(buf.len(), 7);
    }

    #[test]
    fn test_alignment() {
        let mut emitter = AsmDirectiveEmitter::new();
        let mut buf = vec![0x01, 0x02, 0x03];

        emitter.apply_alignment(2, &mut buf); // 4-byte alignment
        assert_eq!(buf.len() % 4, 0);
    }

    #[test]
    fn test_symbol_directives() {
        let mut emitter = AsmDirectiveEmitter::new();
        let mut symbols = Vec::new();

        emitter.emit_globl("main", &mut symbols);
        assert!(symbols.contains(&"main".to_string()));
    }

    #[test]
    fn test_cfi_directives() {
        let emitter = AsmDirectiveEmitter::new();
        assert!(emitter.emit_cfi_startproc().contains("cfi"));
        assert!(emitter.emit_cfi_endproc().contains("cfi"));
    }
}
