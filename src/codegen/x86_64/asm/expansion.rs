// src/codegen/x86_64/asm/expansion.rs
// Assembly template instantiation engine for inline assembly
// Extracted from src/codegen/x86_64/mod.rs (Turns 36-37)

use crate::ast::{Expr, Type};

// Forward declaration of Member type
#[derive(Clone)]
pub struct Member {
    pub field_name: String,
    pub ty: Type,
    pub offset: usize,
}

/// Parse GCC asm constraint flags (=, +, &, %)
/// Returns: (is_output, is_inout, early_clobber, match_digit)
pub fn parse_asm_constraint_flags(constraint: &str) -> (bool, bool, bool, Option<u32>) {
    let mut is_output = false;
    let mut is_inout = false;
    let mut early = false;
    let mut match_digit: Option<u32> = None;
    let mut s = constraint.trim();
    while !s.is_empty() {
        match s.chars().next() {
            Some('=') => {
                is_output = true;
                s = &s[1..];
            },
            Some('+') => {
                is_output = true;
                is_inout = true;
                s = &s[1..];
            },
            Some('&') => {
                early = true;
                s = &s[1..];
            },
            Some('%') => {
                s = &s[1..];
            },
            Some(c) if c.is_ascii_digit() => {
                let mut val: u32 = 0;
                while let Some(d) = s.chars().next() {
                    if !d.is_ascii_digit() {
                        break;
                    }
                    val = val.wrapping_mul(10).wrapping_add(d.to_digit(10).unwrap());
                    s = &s[1..];
                }
                match_digit = Some(val);
            },
            _ => break,
        }
    }
    (is_output, is_inout, early, match_digit)
}

/// Check if constraint prefers memory operand
pub fn asm_constraint_prefers_memory(constraint: &str, is_inout: bool) -> bool {
    if constraint.is_empty() {
        return false;
    }
    let letters: Vec<char> = constraint
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .collect();
    let has_m = letters.contains(&'m');
    // let _has_p = letters.contains(&'p');
    let has_r = letters
        .iter()
        .any(|c| matches!(c, 'r' | 'R' | 'q' | 'Q' | 'a' | 'b' | 'c' | 'd' | 'S' | 'D'));
    if has_m && (!has_r || is_inout) {
        return true;
    }
    letters == vec!['m']
}

/// Check if constraint requires immediate operand
pub fn asm_constraint_is_immediate(constraint: &str) -> bool {
    constraint
        .chars()
        .any(|c| matches!(c, 'i' | 'n' | 'I' | 'N' | 'M' | 'X' | 'E'))
}

/// Check if constraint wants XMM register ("x", "Y", "Yt", "Xf")
pub fn asm_constraint_wants_xmm(constraint: &str) -> Option<u8> {
    let letters: Vec<char> = constraint
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .collect();
    if letters.contains(&'x') {
        return Some(128); // default 128-bit; "Yt" is xmm, "Xf" is 32-bit, "Yx" is 64-bit
    }
    if letters.contains(&'Y') {
        return Some(128);
    }
    None
}

/// Check if constraint requires memory address ("p")
pub fn asm_constraint_is_address(constraint: &str) -> bool {
    constraint.chars().any(|c| c == 'p')
}

/// Check if constraint is general ("g" includes reg/mem/imm)
pub fn asm_constraint_is_general(constraint: &str) -> bool {
    let letters: Vec<char> = constraint
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .collect();
    letters.contains(&'g')
}

/// Check if constraint requires GP register
pub fn asm_constraint_is_gp(constraint: &str) -> bool {
    let letters: Vec<char> = constraint
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .collect();
    letters
        .iter()
        .any(|c| matches!(c, 'r' | 'R' | 'q' | 'Q' | 'a' | 'b' | 'c' | 'd' | 'S' | 'D'))
}

/// x87 FPU constraint: "f" → any st, "t" → st(0), "u" → st(1)
pub fn asm_constraint_wants_x87(constraint: &str) -> Option<u8> {
    let core = constraint.trim_start_matches(['=', '+', '&']);
    if core.contains('{') {
        return None;
    }
    let letters: Vec<char> = core.chars().filter(|c| c.is_ascii_alphabetic()).collect();
    if letters.contains(&'t') {
        return Some(0);
    }
    if letters.contains(&'u') {
        return Some(1);
    }
    if letters.contains(&'f') {
        return Some(0);
    }
    None
}

/// MMX constraint: "y" → mm0..mm7
pub fn asm_constraint_wants_mmx(constraint: &str) -> bool {
    let core = constraint.trim_start_matches(['=', '+', '&']);
    if core.contains('{') {
        return false;
    }
    core.chars()
        .filter(|c| c.is_ascii_alphabetic())
        .any(|c| c == 'y')
}

/// Parse GCC condition code constraint: "@cc<letters>" → return condition code letter sequence
pub fn asm_parse_cc_constraint(constraint: &str) -> Option<&str> {
    let s = constraint.trim_start_matches(['=', '+', '&']);
    s.strip_prefix("@cc").filter(|rest| !rest.is_empty())
}

/// Condition code letter → setcc opcode (0F 90+off)
pub fn cc_letter_to_setcc_opcode(letters: &str) -> Option<u8> {
    let off = match letters {
        "o" => 0x00,
        "no" => 0x01,
        "b" | "c" | "nae" => 0x02,
        "ae" | "nb" | "nc" => 0x03,
        "e" | "z" => 0x04,
        "ne" | "nz" => 0x05,
        "be" | "na" => 0x06,
        "a" | "nbe" => 0x07,
        "s" => 0x08,
        "ns" => 0x09,
        "p" | "pe" => 0x0A,
        "np" | "po" => 0x0B,
        "l" | "nge" => 0x0C,
        "ge" | "nl" => 0x0D,
        "le" | "ng" => 0x0E,
        "g" | "nle" => 0x0F,
        _ => return None,
    };
    Some(0x90 + off)
}

/// Explicit register constraint: "{regname}"
pub fn asm_parse_explicit_reg(constraint: &str) -> Option<ExplicitRegInfo> {
    let start = constraint.find('{')?;
    let end = constraint.find('}')?;
    if end <= start + 1 {
        return None;
    }
    let name = &constraint[start + 1..end];
    parse_explicit_reg_name(name)
}

/// Parse explicit register name
pub fn parse_explicit_reg_name(name: &str) -> Option<ExplicitRegInfo> {
    use crate::codegen::x86_64::asm::expansion::ExplicitRegKind;

    // Segment registers
    match name {
        "gs" => {
            return Some(ExplicitRegInfo {
                reg: 5,
                size: 16,
                att_name: "gs",
                kind: ExplicitRegKind::Seg,
            })
        },
        "fs" => {
            return Some(ExplicitRegInfo {
                reg: 4,
                size: 16,
                att_name: "fs",
                kind: ExplicitRegKind::Seg,
            })
        },
        "es" => {
            return Some(ExplicitRegInfo {
                reg: 0,
                size: 16,
                att_name: "es",
                kind: ExplicitRegKind::Seg,
            })
        },
        "cs" => {
            return Some(ExplicitRegInfo {
                reg: 1,
                size: 16,
                att_name: "cs",
                kind: ExplicitRegKind::Seg,
            })
        },
        "ss" => {
            return Some(ExplicitRegInfo {
                reg: 2,
                size: 16,
                att_name: "ss",
                kind: ExplicitRegKind::Seg,
            })
        },
        "ds" => {
            return Some(ExplicitRegInfo {
                reg: 3,
                size: 16,
                att_name: "ds",
                kind: ExplicitRegKind::Seg,
            })
        },
        _ => {},
    }

    // GP subregisters
    if let Some((idx, bits, sname)) = parse_reg_name_bits(name) {
        return Some(ExplicitRegInfo {
            reg: idx,
            size: bits,
            att_name: sname,
            kind: ExplicitRegKind::Gp,
        });
    }
    None
}

/// Parse register name → (index, width_bits, AT&T name without %)
fn parse_reg_name_bits(name: &str) -> Option<(u8, u8, &'static str)> {
    Some(match name {
        "rax" => (0, 64, "rax"),
        "eax" => (0, 32, "eax"),
        "ax" => (0, 16, "ax"),
        "al" => (0, 8, "al"),
        "ah" => (0, 8, "ah"),
        "rbx" => (3, 64, "rbx"),
        "ebx" => (3, 32, "ebx"),
        "bx" => (3, 16, "bx"),
        "bl" => (3, 8, "bl"),
        "bh" => (3, 8, "bh"),
        "rcx" => (1, 64, "rcx"),
        "ecx" => (1, 32, "ecx"),
        "cx" => (1, 16, "cx"),
        "cl" => (1, 8, "cl"),
        "ch" => (1, 8, "ch"),
        "rdx" => (2, 64, "rdx"),
        "edx" => (2, 32, "edx"),
        "dx" => (2, 16, "dx"),
        "dl" => (2, 8, "dl"),
        "dh" => (2, 8, "dh"),
        "rsi" => (6, 64, "rsi"),
        "esi" => (6, 32, "esi"),
        "si" => (6, 16, "si"),
        "sil" => (6, 8, "sil"),
        "rdi" => (7, 64, "rdi"),
        "edi" => (7, 32, "edi"),
        "di" => (7, 16, "di"),
        "dil" => (7, 8, "dil"),
        "rsp" => (4, 64, "rsp"),
        "esp" => (4, 32, "esp"),
        "sp" => (4, 16, "sp"),
        "spl" => (4, 8, "spl"),
        "rbp" => (5, 64, "rbp"),
        "ebp" => (5, 32, "ebp"),
        "bp" => (5, 16, "bp"),
        "bpl" => (5, 8, "bpl"),
        "r8" => (8, 64, "r8"),
        "r8d" => (8, 32, "r8d"),
        "r8w" => (8, 16, "r8w"),
        "r8b" => (8, 8, "r8b"),
        "r9" => (9, 64, "r9"),
        "r9d" => (9, 32, "r9d"),
        "r9w" => (9, 16, "r9w"),
        "r9b" => (9, 8, "r9b"),
        "r10" => (10, 64, "r10"),
        "r10d" => (10, 32, "r10d"),
        "r10w" => (10, 16, "r10w"),
        "r10b" => (10, 8, "r10b"),
        "r11" => (11, 64, "r11"),
        "r11d" => (11, 32, "r11d"),
        "r11w" => (11, 16, "r11w"),
        "r11b" => (11, 8, "r11b"),
        "r12" => (12, 64, "r12"),
        "r12d" => (12, 32, "r12d"),
        "r12w" => (12, 16, "r12w"),
        "r12b" => (12, 8, "r12b"),
        "r13" => (13, 64, "r13"),
        "r13d" => (13, 32, "r13d"),
        "r13w" => (13, 16, "r13w"),
        "r13b" => (13, 8, "r13b"),
        "r14" => (14, 64, "r14"),
        "r14d" => (14, 32, "r14d"),
        "r14w" => (14, 16, "r14w"),
        "r14b" => (14, 8, "r14b"),
        "r15" => (15, 64, "r15"),
        "r15d" => (15, 32, "r15d"),
        "r15w" => (15, 16, "r15w"),
        "r15b" => (15, 8, "r15b"),
        _ => return None,
    })
}

/// Explicit register info
#[derive(Clone, Copy)]
pub enum ExplicitRegKind {
    Gp,
    Seg,
}

/// Register information for explicit constraints
pub struct ExplicitRegInfo {
    /// Register index (GP: rax=0…; Seg: 0=ES,1=CS,…,4=FS,5=GS)
    pub reg: u8,
    /// Register width in bits: 8/16/32/64
    pub size: u8,
    /// AT&T register name without % prefix
    pub att_name: &'static str,
    /// Register kind
    pub kind: ExplicitRegKind,
}

/// Handle GCC asm template operand modifier placeholders (%bN, %wN, %hN, %kN, %qN, %aN)
/// val is the original value from op_map (register name without % prefix like "rax")
/// Returns the substituted text with % prefix; None if conversion fails
pub fn asm_reg_modifier_subst(val: &str, modifier: char) -> Option<String> {
    let (idx, sz) = parse_asm_reg(val)?;
    if sz >= 128 {
        return None; // Vector/mask registers have no b/w/k/q modifiers
    }
    if modifier == 'h' {
        // High 8-bit registers: only ax/bx/cx/dx
        let name = match idx {
            0 => "%ah",
            1 => "%ch",
            2 => "%dh",
            3 => "%bh",
            _ => return None,
        };
        return Some(name.to_string());
    }
    let bits = match modifier {
        'b' => 8,
        'w' => 16,
        'k' => 32,
        'q' => 64,
        'a' => sz,
        _ => return None,
    };
    gp_reg_name_bits(idx, bits).map(|s| s.to_string())
}

/// Parse ASM register string → (index, size)
pub fn parse_asm_reg(s: &str) -> Option<(u8, u8)> {
    use crate::codegen::x86_64::{
        ASM_REG_CR, ASM_REG_DR, ASM_REG_K, ASM_REG_MMX, ASM_REG_ST, ASM_REG_TMM, ASM_REG_XMM,
        ASM_REG_YMM, ASM_REG_ZMM,
    };

    let n = s.strip_prefix('%').unwrap_or(s);
    let result = match n {
        // General purpose registers
        "rax" => (0, 64),
        "eax" => (0, 32),
        "ax" => (0, 16),
        "al" => (0, 8),
        "rbx" => (3, 64),
        "ebx" => (3, 32),
        "bx" => (3, 16),
        "bl" => (3, 8),
        "rcx" => (1, 64),
        "ecx" => (1, 32),
        "cx" => (1, 16),
        "cl" => (1, 8),
        "rdx" => (2, 64),
        "edx" => (2, 32),
        "dx" => (2, 16),
        "dl" => (2, 8),
        "rsi" => (6, 64),
        "esi" => (6, 32),
        "si" => (6, 16),
        "sil" => (6, 8),
        "rdi" => (7, 64),
        "edi" => (7, 32),
        "di" => (7, 16),
        "dil" => (7, 8),
        "rsp" => (4, 64),
        "esp" => (4, 32),
        "sp" => (4, 16),
        "spl" => (4, 8),
        "rbp" => (5, 64),
        "ebp" => (5, 32),
        "bp" => (5, 16),
        "bpl" => (5, 8),
        "r8" => (8, 64),
        "r8d" => (8, 32),
        "r8w" => (8, 16),
        "r8b" => (8, 8),
        "r9" => (9, 64),
        "r9d" => (9, 32),
        "r9w" => (9, 16),
        "r9b" => (9, 8),
        "r10" => (10, 64),
        "r10d" => (10, 32),
        "r10w" => (10, 16),
        "r10b" => (10, 8),
        "r11" => (11, 64),
        "r11d" => (11, 32),
        "r11w" => (11, 16),
        "r11b" => (11, 8),
        "r12" => (12, 64),
        "r12d" => (12, 32),
        "r12w" => (12, 16),
        "r12b" => (12, 8),
        "r13" => (13, 64),
        "r13d" => (13, 32),
        "r13w" => (13, 16),
        "r13b" => (13, 8),
        "r14" => (14, 64),
        "r14d" => (14, 32),
        "r14w" => (14, 16),
        "r14b" => (14, 8),
        "r15" => (15, 64),
        "r15d" => (15, 32),
        "r15w" => (15, 16),
        "r15b" => (15, 8),
        // Subregisters (high bytes)
        "ah" => (0, 8),
        "bh" => (3, 8),
        "ch" => (1, 8),
        "dh" => (2, 8),
        // SIMD registers
        x if x.starts_with("xmm") => {
            let idx = x.strip_prefix("xmm")?.parse::<u8>().ok()?;
            (idx, ASM_REG_XMM)
        },
        x if x.starts_with("ymm") => {
            let idx = x.strip_prefix("ymm")?.parse::<u8>().ok()?;
            (idx, ASM_REG_YMM)
        },
        x if x.starts_with("zmm") => {
            let idx = x.strip_prefix("zmm")?.parse::<u8>().ok()?;
            (idx, ASM_REG_ZMM)
        },
        x if x.starts_with("tmm") => {
            let idx = x.strip_prefix("tmm")?.parse::<u8>().ok()?;
            (idx, ASM_REG_TMM)
        },
        // Mask registers
        x if x.starts_with("k") => {
            let idx = x.strip_prefix("k")?.parse::<u8>().ok()?;
            (idx, ASM_REG_K)
        },
        // Control registers
        x if x.starts_with("cr") => {
            let idx = x.strip_prefix("cr")?.parse::<u8>().ok()?;
            (idx, ASM_REG_CR)
        },
        // Debug registers
        x if x.starts_with("dr") || x.starts_with("db") => {
            let idx = x
                .strip_prefix("dr")
                .or_else(|| x.strip_prefix("db"))?
                .parse::<u8>()
                .ok()?;
            (idx, ASM_REG_DR)
        },
        // MMX registers
        x if x.starts_with("mm") => {
            let idx = x.strip_prefix("mm")?.parse::<u8>().ok()?;
            (idx, ASM_REG_MMX)
        },
        // x87 FPU stack
        "st" | "st(0)" => (0, ASM_REG_ST),
        "st(1)" => (1, ASM_REG_ST),
        "st(2)" => (2, ASM_REG_ST),
        "st(3)" => (3, ASM_REG_ST),
        "st(4)" => (4, ASM_REG_ST),
        "st(5)" => (5, ASM_REG_ST),
        "st(6)" => (6, ASM_REG_ST),
        "st(7)" => (7, ASM_REG_ST),
        _ => return None,
    };
    Some(result)
}

/// Select single constraint from multi-constraint string (e.g., "rm")
pub fn asm_pick_sub_constraint(constraint: &str, prefer_memory: bool) -> &'static str {
    let letters: Vec<char> = constraint
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .collect();
    if prefer_memory && letters.contains(&'m') {
        return "m";
    }
    if letters.contains(&'r') {
        return "r";
    }
    if letters.contains(&'m') {
        return "m";
    }
    // Immediate constraint: i (any) / n / I/J/K/L/M/N/O (displacement) / X / E/F/G/H (floating point)
    if letters.iter().any(|c| {
        matches!(
            c,
            'i' | 'n' | 'I' | 'J' | 'K' | 'L' | 'M' | 'N' | 'O' | 'X' | 'E' | 'F' | 'G' | 'H' | 's'
        )
    }) {
        return "i";
    }
    if letters.contains(&'g') {
        return "g";
    }
    "r"
}

/// Format ASM symbol with immediate offset
pub fn format_asm_symbol_immediate(name: &str, offset: i128) -> String {
    match offset.cmp(&0) {
        std::cmp::Ordering::Greater => format!("{name}+{offset}"),
        std::cmp::Ordering::Less => format!("{name}{offset}"),
        std::cmp::Ordering::Equal => name.to_string(),
    }
}

/// Evaluate ASM operand for constant values - placeholder implementation
pub fn eval_asm_operand(_expr: &Expr) -> Option<String> {
    // This function would need full AST access and const evaluation logic
    // Placeholder returns None for now - actual implementation in mod.rs
    None
}

/// Compute offset for offsetof expression - placeholder
fn compute_offset(_ty: &Type, _member: &[Member]) -> usize {
    0
}

/// Check if line has unresolved operands
pub fn asm_has_unresolved_operand(line: &str) -> bool {
    line.contains("%[")
        || line
            .as_bytes()
            .windows(2)
            .any(|w| w[0] == b'%' && w[1].is_ascii_digit())
}

/// Check if size is valid GPR
pub fn asm_reg_is_gpr(sz: u8) -> bool {
    sz <= 64
}

/// Helper function to get register name in AT&T format
pub fn gp_reg_name_bits(idx: u8, bits: u8) -> Option<&'static str> {
    Some(match (idx, bits) {
        (0, 8) => "%al",
        (0, 16) => "%ax",
        (0, 32) => "%eax",
        (0, 64) => "%rax",
        (1, 8) => "%cl",
        (1, 16) => "%cx",
        (1, 32) => "%ecx",
        (1, 64) => "%rcx",
        (2, 8) => "%dl",
        (2, 16) => "%dx",
        (2, 32) => "%edx",
        (2, 64) => "%rdx",
        (3, 8) => "%bl",
        (3, 16) => "%bx",
        (3, 32) => "%ebx",
        (3, 64) => "%rbx",
        (4, 8) => "%spl",
        (4, 16) => "%sp",
        (4, 32) => "%esp",
        (4, 64) => "%rsp",
        (5, 8) => "%bpl",
        (5, 16) => "%bp",
        (5, 32) => "%ebp",
        (5, 64) => "%rbp",
        (6, 8) => "%sil",
        (6, 16) => "%si",
        (6, 32) => "%esi",
        (6, 64) => "%rsi",
        (7, 8) => "%dil",
        (7, 16) => "%di",
        (7, 32) => "%edi",
        (7, 64) => "%rdi",
        (8, 8) => "%r8b",
        (8, 16) => "%r8w",
        (8, 32) => "%r8d",
        (8, 64) => "%r8",
        (9, 8) => "%r9b",
        (9, 16) => "%r9w",
        (9, 32) => "%r9d",
        (9, 64) => "%r9",
        (10, 8) => "%r10b",
        (10, 16) => "%r10w",
        (10, 32) => "%r10d",
        (10, 64) => "%r10",
        (11, 8) => "%r11b",
        (11, 16) => "%r11w",
        (11, 32) => "%r11d",
        (11, 64) => "%r11",
        (12, 8) => "%r12b",
        (12, 16) => "%r12w",
        (12, 32) => "%r12d",
        (12, 64) => "%r12",
        (13, 8) => "%r13b",
        (13, 16) => "%r13w",
        (13, 32) => "%r13d",
        (13, 64) => "%r13",
        (14, 8) => "%r14b",
        (14, 16) => "%r14w",
        (14, 32) => "%r14d",
        (14, 64) => "%r14",
        (15, 8) => "%r15b",
        (15, 16) => "%r15w",
        (15, 32) => "%r15d",
        (15, 64) => "%r15",
        _ => return None,
    })
}
