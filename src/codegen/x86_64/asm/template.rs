// src/codegen/x86_64/asm/template.rs
// Inline assembly template resolution for x86_64 backend
// Handles placeholder substitution in asm templates

/// Expand inline assembly local labels (1f, 1b notation) in instruction lines
pub struct AsmLocalLabels;

impl AsmLocalLabels {
    /// Expand local label references (e.g., 1f = next label "1:", 1b = previous label "1:")
    pub fn expand_label_insn_lines(lines: Vec<String>) -> Vec<String> {
        let mut result = Vec::new();
        let mut label_map: HashMap<String, usize> = HashMap::new();
        let mut label_counter = 0;
        
        for line in lines {
            let expanded = Self::expand_local_labels(&line, &label_map);
            result.push(expanded.clone());
            
            // Track named labels for forward/backward reference
            if let Some(label) = Self::parse_asm_named_label(&line) {
                if !label.chars().all(|c| c.is_ascii_digit()) {
                    label_map.insert(label.to_string(), label_counter);
                    label_counter += 1;
                }
            }
        }
        
        result
    }

    /// Parse named labels. Supports `foo:` and `foo: insn`, excludes numeric local labels and directives
    pub fn parse_asm_named_label(line: &str) -> Option<&str> {
        let line = Self::strip_asm_comment(line).trim();
        let (label, _) = line.split_once(':')?;
        let label = label.trim();
        
        if label.is_empty()
            || label.chars().all(|c| c.is_ascii_digit())
            || Self::is_asm_directive(label)
            || label.contains([' ', '\t', '%', '(', ')'])
            || label.starts_with('.')
        {
            return None;
        }
        
        Some(label)
    }

    /// Check if name is an assembly directive (not a local label)
    pub fn is_asm_directive(name: &str) -> bool {
        matches!(
            name,
            ".pushsection"
                | ".popsection"
                | ".section"
                | ".previous"
                | ".type"
                | ".size"
                | ".ident"
                | ".globl"
                | ".global"
                | ".weak"
                | ".hidden"
                | ".protected"
                | ".balign"
                | ".p2align"
                | ".align"
                | ".long"
                | ".quad"
                | ".byte"
                | ".asciz"
                | ".string"
                | ".text"
                | ".data"
                | ".bss"
                | ".rodata"
                | ".org"
                | ".zero"
                | ".skip"
                | ".set"
                | ".equ"
                | ".macro"
                | ".endm"
                | ".if"
                | ".endif"
                | ".else"
                | ".elseif"
                | ".rept"
                | ".endr"
                | ".irp"
                | ".irpc"
                | ".loc"
                | ".file"
                | ".symver"
                | ".local"
                | ".comm"
                | ".lcomm"
                | ".altmacro"
                | ".code64"
                | ".subsection"
                | ".uleb128"
                | ".sleb128"
                | ".2byte"
                | ".4byte"
                | ".8byte"
                | ".word"
                | ".int"
                | ".error"
                | ".warning"
                | ".include"
                | ".incbin"
                | ".cfi_startproc"
                | ".cfi_endproc"
                | ".cfi_def_cfa_offset"
                | ".cfi_def_cfa"
                | ".cfi_offset"
                | ".cfi_restore"
                | ".cfi_remember_state"
                | ".cfi_restore_state"
                | ".cfi_adjust_cfa_offset"
                | ".cfi_def_cfa_register"
                | ".cfi_return_column"
                | ".cfi_signal_frame"
                | ".cfi_window_save"
                | ".cfi_negate_ra_state"
        )
    }

    /// Strip assembly comment from line
    pub fn strip_asm_comment(s: &str) -> &str {
        s.split('#').next().unwrap_or(s)
    }

    /// Expand local label references like "1f" (forward), "1b" (backward), "2f", etc.
    fn expand_local_labels(line: &str, label_map: &HashMap<String, usize>) -> String {
        let mut result = line.to_string();
        
        // Find all local label references: number followed by f/b prefix
        let pattern = regex_local_label();
        if let Some(cap) = pattern.find(line) {
            let ref_str = cap.as_str();
            if let (Some(num), Some(direction)) = ref_str.split_at(ref_str.len() - 1).split_once(|c: char| c.is_alphabetic()) {
                match direction {
                    "f" => {
                        // Forward reference: find next matching label
                        if let Some(&idx) = label_map.get(num) {
                            result = result.replace(ref_str, &format!("{}:", num));
                        }
                    }
                    "b" => {
                        // Backward reference: use existing mapping
                        if let Some(&idx) = label_map.get(num) {
                            result = result.replace(ref_str, &format!("{}:", num));
                        }
                    }
                    _ => {}
                }
            }
        }
        
        result
    }
}

// Simple regex pattern for local label references (number+f/b)
fn regex_local_label() -> std::regex::Regex {
    static INIT: std::sync::Once = std::sync::Once::new();
    static mut REGEX: Option<std::regex::Regex> = None;
    
    INIT.call_once(|| {
        unsafe {
            REGEX = Some(std::regex::Regex::new(r"\d+[fb]").unwrap());
        }
    });
    
    unsafe { REGEX.clone().unwrap() }
}
