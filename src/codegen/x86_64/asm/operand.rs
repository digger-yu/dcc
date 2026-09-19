// src/codegen/x86_64/asm/operand.rs
// Inline assembly operand parsing and representation for x86_64 backend

/// Assembly register base constants
pub const ASM_BASE_RIP: u8 = 255;
pub const ASM_REG_XMM: u8 = 128;
pub const ASM_REG_YMM: u8 = 129;
pub const ASM_REG_ZMM: u8 = 130;
pub const ASM_REG_TMM: u8 = 131;
pub const ASM_REG_K: u8 = 132;
pub const ASM_REG_CR: u8 = 133;
pub const ASM_REG_DR: u8 = 134;
pub const ASM_REG_MMX: u8 = 135;
pub const ASM_REG_ST: u8 = 136;

/// Assembly operand types parsed from AT&T syntax
#[derive(Debug, Clone, PartialEq)]
pub enum AsmOperand {
    None,
    Reg(u8, u8),      // (index, size_bits)
    Mem {
        disp: i32,
        base: u8,
        index: Option<u8>,
        scale: u8,
    },
    Sym(String),           // Symbol reference
    SymDisp(String, i32),  // Symbol with displacement
    Imm(i64),              // Immediate value
    Seg(u8),               // Segment code
    SegMem {
        seg: u8,
        disp: i32,
        base: u8,
        index: Option<u8>,
        scale: u8,
    },
}

/// Parse AT&T string operand into AsmOperand structure
pub fn parse_asm_operand_str(s: &str) -> AsmOperand {
    let s = s.trim();
    if s.is_empty() {
        return AsmOperand::None;
    }
    
    // Strip segment prefix: %gs:... / gs:... / %gs / gs (must be before other checks)
    let (s, seg) = strip_asm_seg_prefix(s);
    let s = s.trim();
    
    if s.is_empty() {
        return if let Some(sc) = seg {
            AsmOperand::Seg(sc)
        } else {
            AsmOperand::None
        };
    }
    
    // Indirect prefix *: jmp *%rax, call *symbol, jmp *(%rax), etc.
    if let Some(rest) = s.strip_prefix('*') {
        let inner = parse_asm_operand_str(rest);
        return wrap_indirect(inner);
    }
    
    // Immediate: $N
    if let Some(rest) = s.strip_prefix('$') {
        let rest = rest.trim();
        if let Some(hex) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
            if let Ok(v) = i64::from_str_radix(hex, 16) {
                return AsmOperand::Imm(v);
            }
        } else if let Ok(v) = rest.parse::<i64>() {
            return AsmOperand::Imm(v);
        }
        // $label (e.g., $1f, $1b) - local label reference
        return AsmOperand::Sym(rest.to_string());
    }
    
    // Register: %reg or bare rax/rbx, etc.
    if s.starts_with('%') {
        if let Some((idx, sz)) = parse_asm_reg(s) {
            return AsmOperand::Reg(idx, sz);
        }
        return AsmOperand::None;
    }
    
    if let Some((idx, sz)) = parse_asm_reg(&format!("%{}", s)) {
        return AsmOperand::Reg(idx, sz);
    }
    
    // Memory: disp(%base,%idx,scale) or (%reg) or symbol(%rip)
    if s.contains('(') {
        let inner = parse_asm_mem(s);
        return if let (Some(sc), AsmOperand::Mem { disp, base, index, scale }) = (seg, &inner) {
            AsmOperand::SegMem {
                seg: sc,
                disp,
                base,
                index,
                scale,
            }
        } else {
            inner
        };
    }
    
    // Segment prefix + symbol (e.g., `gs:symbol` which is rare; treat as symbol)
    // Symbol name (global tag reference) + segment prefix → segment prefix follows to emit phase
    let sym = strip_asm_comment(s).trim().to_string();
    if let Some(sc) = seg {
        return AsmOperand::SegMem {
            seg: sc,
            disp: 0,
            base: ASM_BASE_RIP,
            index: None,
            scale: 1,
        };
    }
    
    AsmOperand::Sym(sym)
}

/// Strip segment prefix from string
fn strip_asm_seg_prefix(s: &str) -> (&str, Option<u8>) {
    let s = s.trim_start();
    let (head, rest) = if let Some(after) = s.strip_prefix('%') {
        // Find first non-alphabetic character as head/rest split
        let pos = after.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(after.len());
        (&after[..pos], &after[pos..])
    } else {
        let pos = s.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(s.len());
        (&s[..pos], &s[pos..])
    };
    
    if let Some((code, _pfx)) = parse_asm_seg(head) {
        if let Some(after_colon) = rest.strip_prefix(':') {
            return (after_colon, Some(code));
        }
        // Standalone segment name: entire s has been consumed as segment name
        if rest.is_empty() {
            return ("", Some(code));
        }
    }
    
    (s, None)
}

/// Parse segment register name → (seg_code, prefix_byte)
/// seg_code for modrm reg field: 0=ES,1=CS,2=SS,3=DS,4=FS,5=GS
/// prefix_byte for mov/push segment prefix
fn parse_asm_seg(name: &str) -> Option<(u8, u8)> {
    let n = name.strip_prefix('%').unwrap_or(name);
    match n {
        "es" => Some((0, 0x26)),
        "cs" => Some((1, 0x2E)),
        "ss" => Some((2, 0x36)),
        "ds" => Some((3, 0x3E)),
        "fs" => Some((4, 0x64)),
        "gs" => Some((5, 0x65)),
        _ => None,
    }
}

/// Parse AT&T register name → (reg_index, size_bits)
fn parse_asm_reg(name: &str) -> Option<(u8, u8)> {
    let n = name.strip_prefix('%').unwrap_or(name);
    match n {
        // General purpose registers
        "rax" | "eax" | "ax" | "al" | "ah" => Some((0, 64)),
        "rbx" | "ebx" | "bx" | "bl" | "bh" => Some((3, 64)),
        "rcx" | "ecx" | "cx" | "cl" | "ch" => Some((1, 64)),
        "rdx" | "edx" | "dx" | "dl" | "dh" => Some((2, 64)),
        "rsi" | "esi" | "si" | "sil" => Some((6, 64)),
        "rdi" | "edi" | "di" | "dil" => Some((7, 64)),
        "rsp" | "esp" | "sp" | "spl" => Some((4, 64)),
        "rbp" | "ebp" | "bp" | "bpl" => Some((5, 64)),
        // R8-R15
        "r8" | "r8d" | "r8w" | "r8b" => Some((8, 64)),
        "r9" | "r9d" | "r9w" | "r9b" => Some((9, 64)),
        "r10" | "r10d" | "r10w" | "r10b" => Some((10, 64)),
        "r11" | "r11d" | "r11w" | "r11b" => Some((11, 64)),
        "r12" | "r12d" | "r12w" | "r12b" => Some((12, 64)),
        "r13" | "r13d" | "r13w" | "r13b" => Some((13, 64)),
        "r14" | "r14d" | "r14w" | "r14b" => Some((14, 64)),
        "r15" | "r15d" | "r15w" | "r15b" => Some((15, 64)),
        // APX EGPR r16-r31
        "r16" | "r16d" | "r16w" | "r16b" => Some((16, 64)),
        "r17" | "r17d" | "r17w" | "r17b" => Some((17, 64)),
        "r18" | "r18d" | "r18w" | "r18b" => Some((18, 64)),
        "r19" | "r19d" | "r19w" | "r19b" => Some((19, 64)),
        "r20" | "r20d" | "r20w" | "r20b" => Some((20, 64)),
        "r21" | "r21d" | "r21w" | "r21b" => Some((21, 64)),
        "r22" | "r22d" | "r22w" | "r22b" => Some((22, 64)),
        "r23" | "r23d" | "r23w" | "r23b" => Some((23, 64)),
        "r24" | "r24d" | "r24w" | "r24b" => Some((24, 64)),
        "r25" | "r25d" | "r25w" | "r25b" => Some((25, 64)),
        "r26" | "r26d" | "r26w" | "r26b" => Some((26, 64)),
        "r27" | "r27d" | "r27w" | "r27b" => Some((27, 64)),
        "r28" | "r28d" | "r28w" | "r28b" => Some((28, 64)),
        "r29" | "r29d" | "r29w" | "r29b" => Some((29, 64)),
        "r30" | "r30d" | "r30w" | "r30b" => Some((30, 64)),
        "r31" | "r31d" | "r31w" | "r31b" => Some((31, 64)),
        // RIP
        "rip" => Some((ASM_BASE_RIP, 64)),
        // XMM/YMM/ZMM vector registers
        "xmm0".."xmm15" => {
            let idx = n[3..].parse::<u8>().ok()?;
            Some((idx, ASM_REG_XMM))
        }
        "ymm0".."ymm15" => {
            let idx = n[3..].parse::<u8>().ok()?;
            Some((idx, ASM_REG_YMM))
        }
        "zmm0".."zmm31" => {
            let idx = n[3..].parse::<u8>().ok()?;
            Some((idx, ASM_REG_ZMM))
        }
        // AMX tile registers
        "tmm0".."tmm7" => {
            let idx = n[3..].parse::<u8>().ok()?;
            Some((idx, ASM_REG_TMM))
        }
        // AVX-512 mask registers
        "k0".."k7" => {
            let idx = n[1..].parse::<u8>().ok()?;
            Some((idx, ASM_REG_K))
        }
        // Control registers
        "cr0".."cr8" => {
            let idx = n[2..].parse::<u8>().ok()?;
            Some((idx, ASM_REG_CR))
        }
        // Debug registers
        "dr0".."dr7" | "db0".."db7" => {
            let idx = n[2..3].parse::<u8>().ok()?;
            Some((idx, ASM_REG_DR))
        }
        // MMX registers
        "mm0".."mm7" => {
            let idx = n[2..].parse::<u8>().ok()?;
            Some((idx, ASM_REG_MMX))
        }
        // x87 FPU stack registers
        "st" | "st(0)" => Some((0, ASM_REG_ST)),
        "st(1)" => Some((1, ASM_REG_ST)),
        "st(2)" => Some((2, ASM_REG_ST)),
        "st(3)" => Some((3, ASM_REG_ST)),
        "st(4)" => Some((4, ASM_REG_ST)),
        "st(5)" => Some((5, ASM_REG_ST)),
        "st(6)" => Some((6, ASM_REG_ST)),
        "st(7)" => Some((7, ASM_REG_ST)),
        _ => None,
    }
}

/// Parse AT&T memory operand
fn parse_asm_mem(s: &str) -> AsmOperand {
    // Format: [disp](%base[,%idx[,scale]])
    let paren_pos = s.find('(').unwrap_or(s.len());
    let disp_str = s[..paren_pos].trim();
    let rest = &s[paren_pos..];
    
    let mut disp: i32 = 0;
    if !disp_str.is_empty() {
        if let Some(hex) = disp_str.strip_prefix("0x").or_else(|| disp_str.strip_prefix("0X")) {
            disp = i32::from_str_radix(hex, 16).unwrap_or(0);
        } else if disp_str.parse::<i32>().is_err() {
            // Symbol name: convert to SymDisp with symbol name (RIP-relative default)
            return AsmOperand::SymDisp(disp_str.to_string(), 0);
        } else {
            disp = disp_str.parse::<i32>().unwrap_or(0);
        }
    }
    
    // Extract content inside parentheses
    let inner = if rest.starts_with('(') && rest.ends_with(')') {
        &rest[1..rest.len() - 1]
    } else {
        return AsmOperand::None;
    };
    
    let parts: Vec<&str> = inner.split(',').collect();
    let base_str = parts.first().map(|s| s.trim()).unwrap_or("");
    
    if base_str == "%rip" || base_str == "rip" {
        // Check for index/scale: disp(%rip,%idx,scale)
        let mut idx: Option<u8> = None;
        let mut scale: u8 = 1;
        
        if parts.len() > 1 {
            let idx_str = parts[1].trim();
            if !idx_str.is_empty() {
                idx = parse_asm_reg(idx_str).map(|(r, _)| r);
            }
        }
        
        if parts.len() > 2 {
            scale = parts[2].trim().parse::<u8>().unwrap_or(1);
        }
        
        return AsmOperand::Mem {
            disp,
            base: ASM_BASE_RIP,
            index: idx,
            scale,
        };
    }
    
    let (base, _bsz) = parse_asm_reg(base_str).unwrap_or((5, 64)); // Default rbp
    
    let mut idx: Option<u8> = None;
    let mut scale: u8 = 1;
    
    if parts.len() > 1 {
        let idx_str = parts[1].trim();
        if !idx_str.is_empty() {
            idx = parse_asm_reg(idx_str).map(|(r, _)| r);
        }
    }
    
    if parts.len() > 2 {
        scale = parts[2].trim().parse::<u8>().unwrap_or(1);
    }
    
    AsmOperand::Mem {
        disp,
        base,
        index: idx,
        scale,
    }
}

/// Mark AsmOperand as indirect (jmp/call *x meaning)
fn wrap_indirect(inner: AsmOperand) -> AsmOperand {
    // Indirect Reg → keep Reg, recognize in emit phase (jmp *%rax = jmp %rax)
    // Indirect Mem → keep Mem  
    // Indirect Sym → mark as SymDisp(sym, 0)表示间接跳转
    match inner {
        AsmOperand::Sym(name) => AsmOperand::SymDisp(name, 0),
        other => other, // Reg/Mem return as-is
    }
}

/// Strip assembly comment from string
pub fn strip_asm_comment(s: &str) -> &str {
    s.split('#').next().unwrap_or(s)
}
